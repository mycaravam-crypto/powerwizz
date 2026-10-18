//! Capacity discovery: raise the user count stage by stage until a threshold is missed.
//!
//! The engine fixes the user count at start, so every stage is a separate, ordinary test run (`engine::start`) with
//! `users = n` and `duration = stage_duration`; stages run one after another. Bounded on purpose: every stage stays
//! within `MAX_USERS`, all stages together within `MAX_DURATION_SECS`, and the first missed threshold, failed stage
//! or stop ends the discovery. Without a threshold there is nothing to discover, so one is required.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::engine::{self, Shared};
use crate::model::{Account, MAX_DURATION_SECS, MAX_USERS, TestDefinition};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub start_users: u32,
    pub max_users: u32,
    pub step_size: u32,
    pub stage_duration_secs: u64,
    pub max_error_rate: Option<f64>,
    pub max_p95_ms: Option<f64>,
}

impl Config {
    /// Returns all validation errors; empty means valid.
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if !(1..=MAX_USERS).contains(&self.start_users) {
            errs.push(format!("Start users must be 1..={MAX_USERS}"));
        }
        if !(self.start_users..=MAX_USERS).contains(&self.max_users) {
            errs.push(format!(
                "Max users must be {}..={MAX_USERS}",
                self.start_users
            ));
        }
        if self.step_size == 0 {
            errs.push("Step size must be at least 1".into());
        }
        if !(1..=MAX_DURATION_SECS).contains(&self.stage_duration_secs) {
            errs.push(format!("Stage duration must be 1..={MAX_DURATION_SECS} s"));
        }
        if self.max_error_rate.is_none() && self.max_p95_ms.is_none() {
            errs.push(
                "Capacity discovery needs a threshold: --max-error-rate and/or --max-p95".into(),
            );
        }
        if errs.is_empty() {
            let total = self.stages().len() as u64 * self.stage_duration_secs;
            if total > MAX_DURATION_SECS {
                errs.push(format!(
                    "All stages together take {total} s, more than the {MAX_DURATION_SECS} s limit: \
                     use fewer stages (larger step) or shorter stages"
                ));
            }
        }
        errs
    }

    /// User count of every planned stage: start, start + step, ..., the last one exactly `max_users`.
    /// Only meaningful for a valid config.
    pub fn stages(&self) -> Vec<u32> {
        let mut users = vec![self.start_users];
        while let Some(&last) = users.last().filter(|&&u| u < self.max_users) {
            users.push(
                last.saturating_add(self.step_size.max(1))
                    .min(self.max_users),
            );
        }
        users
    }
}

/// The definition for one stage: the shared test, with this stage's users and duration.
fn stage_definition(def: &TestDefinition, cfg: &Config, users: u32) -> TestDefinition {
    TestDefinition {
        users,
        duration_secs: cfg.stage_duration_secs,
        ..def.clone()
    }
}

/// Everything that would stop the discovery from starting: the config's errors, and what `engine::check` refuses
/// for the largest stage (accounts per user, ramp-up longer than a stage, ...). Empty means startable.
pub fn check(def: &TestDefinition, accounts: &[Account], cfg: &Config) -> Vec<String> {
    let mut errs = cfg.validate();
    if errs.is_empty() {
        errs.extend(engine::check(
            &stage_definition(def, cfg, cfg.max_users),
            accounts,
        ));
    }
    errs
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageResult {
    Passed,
    ThresholdExceeded,
    Cancelled,
    Failed,
}

impl StageResult {
    pub fn as_str(self) -> &'static str {
        match self {
            StageResult::Passed => "passed",
            StageResult::ThresholdExceeded => "threshold_exceeded",
            StageResult::Cancelled => "cancelled",
            StageResult::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    MaxUsersReached,
    ThresholdExceeded,
    Cancelled,
    Failed,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::MaxUsersReached => "max_users_reached",
            StopReason::ThresholdExceeded => "threshold_exceeded",
            StopReason::Cancelled => "cancelled",
            StopReason::Failed => "failed",
        }
    }
}

/// What one stage measured, from the engine's existing metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub requests: u64,
    pub rps: f64,
    pub p95_ms: f64,
    pub error_rate: f64,
    pub users_started: u32,
    pub users_logged_in: u32,
}

/// The verdict on a finished stage, with a human-readable reason. Same rules as the fixed-load thresholds: a stage
/// without requests or with failed logins measured less load than it claims, so it fails instead of passing.
pub fn evaluate(cfg: &Config, m: &Measured) -> (StageResult, String) {
    if m.requests == 0 {
        return (StageResult::Failed, "no request completed".into());
    }
    if m.users_logged_in < m.users_started {
        return (
            StageResult::Failed,
            format!(
                "only {} of {} users logged in",
                m.users_logged_in, m.users_started
            ),
        );
    }
    if let Some(max) = cfg.max_error_rate.filter(|max| m.error_rate > *max) {
        return (
            StageResult::ThresholdExceeded,
            format!(
                "error rate {:.2} % > {:.2} %",
                m.error_rate * 100.0,
                max * 100.0
            ),
        );
    }
    if let Some(max) = cfg.max_p95_ms.filter(|max| m.p95_ms > *max) {
        return (
            StageResult::ThresholdExceeded,
            format!("p95 {:.1} ms > {max:.1} ms", m.p95_ms),
        );
    }
    (StageResult::Passed, "thresholds met".into())
}

#[derive(Clone, Debug)]
pub struct Stage {
    pub users: u32,
    pub measured: Measured,
    pub result: StageResult,
    pub detail: String,
}

pub struct Outcome {
    pub stages: Vec<Stage>,
    pub stop_reason: StopReason,
    /// The last stage's engine state, for the regular report and exports. `None` only if no stage ran.
    pub last: Option<Arc<Shared>>,
}

impl Outcome {
    /// The most users a stage passed with.
    pub fn highest_passed_users(&self) -> Option<u32> {
        self.stages
            .iter()
            .filter(|s| s.result == StageResult::Passed)
            .map(|s| s.users)
            .max()
    }

    /// The `capacity` section of the JSON report.
    pub fn to_json(&self, cfg: &Config) -> Value {
        let ms = |v: f64| (v * 1e3).round() / 1e3;
        json!({
            "config": {
                "start_users": cfg.start_users,
                "max_users": cfg.max_users,
                "step_size": cfg.step_size,
                "stage_duration_s": cfg.stage_duration_secs,
                "max_error_rate": cfg.max_error_rate,
                "max_p95_latency_ms": cfg.max_p95_ms,
            },
            "stages": self.stages.iter().map(|s| json!({
                "users": s.users,
                "requests": s.measured.requests,
                "rps": s.measured.rps,
                "p95_ms": ms(s.measured.p95_ms),
                "error_rate": s.measured.error_rate,
                "result": s.result.as_str(),
                "detail": s.detail,
            })).collect::<Vec<_>>(),
            "highest_passed_users": self.highest_passed_users(),
            "stop_reason": self.stop_reason.as_str(),
        })
    }
}

fn measure(shared: &Shared) -> Measured {
    let m = shared.metrics.lock().unwrap();
    Measured {
        requests: m.requests,
        rps: m.requests as f64 / shared.runtime().as_secs_f64().max(0.001),
        p95_ms: m.latency.percentile(0.95).as_secs_f64() * 1e3,
        error_rate: m.error_rate(),
        users_started: shared.started.load(Ordering::Relaxed),
        users_logged_in: shared.logged_in.load(Ordering::Relaxed),
    }
}

/// Runs the stages one after another until a threshold is missed, a stage fails, `max_users` passed, or `cancel`
/// turns true (the running stage is stopped and recorded as cancelled; no further stage starts). `on_stage` sees
/// each finished stage. Must be called inside a tokio runtime.
pub async fn run(
    def: TestDefinition,
    accounts: Vec<Account>,
    keep_records: bool,
    cfg: &Config,
    mut cancel: watch::Receiver<bool>,
    mut on_stage: impl FnMut(&Stage),
) -> Result<Outcome, Vec<String>> {
    let errs = check(&def, &accounts, cfg);
    if !errs.is_empty() {
        return Err(errs);
    }
    let mut outcome = Outcome {
        stages: Vec::new(),
        stop_reason: StopReason::MaxUsersReached,
        last: None,
    };
    for users in cfg.stages() {
        if *cancel.borrow() {
            outcome.stop_reason = StopReason::Cancelled;
            break;
        }
        let mut h = engine::start(
            stage_definition(&def, cfg, users),
            accounts.clone(),
            keep_records,
        )?;
        let cancelled = tokio::select! {
            _ = h.wait() => false,
            Ok(_) = cancel.wait_for(|c| *c) => {
                h.stop();
                h.wait().await;
                true
            }
        };
        let measured = measure(&h.shared);
        let (result, detail) = if cancelled {
            (
                StageResult::Cancelled,
                "stopped before the stage ended".into(),
            )
        } else {
            evaluate(cfg, &measured)
        };
        let stage = Stage {
            users,
            measured,
            result,
            detail,
        };
        on_stage(&stage);
        outcome.stages.push(stage);
        outcome.last = Some(h.shared.clone());
        outcome.stop_reason = match result {
            StageResult::Passed => continue,
            StageResult::ThresholdExceeded => StopReason::ThresholdExceeded,
            StageResult::Cancelled => StopReason::Cancelled,
            StageResult::Failed => StopReason::Failed,
        };
        break;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(start: u32, max: u32, step: u32) -> Config {
        Config {
            start_users: start,
            max_users: max,
            step_size: step,
            stage_duration_secs: 10,
            max_error_rate: Some(0.01),
            max_p95_ms: Some(200.0),
        }
    }

    fn measured(error_rate: f64, p95_ms: f64) -> Measured {
        Measured {
            requests: 100,
            rps: 10.0,
            p95_ms,
            error_rate,
            users_started: 5,
            users_logged_in: 5,
        }
    }

    #[test]
    fn stages_step_up_and_end_exactly_at_max() {
        assert_eq!(cfg(10, 40, 10).stages(), [10, 20, 30, 40]);
        assert_eq!(cfg(10, 45, 10).stages(), [10, 20, 30, 40, 45]);
        assert_eq!(cfg(5, 5, 10).stages(), [5]);
        assert_eq!(cfg(1, MAX_USERS, MAX_USERS).stages(), [1, MAX_USERS]);
    }

    #[test]
    fn rejects_invalid_or_unbounded_configs() {
        let bad = [
            Config {
                start_users: 0,
                ..cfg(1, 10, 1)
            },
            Config {
                max_users: MAX_USERS + 1,
                ..cfg(1, 10, 1)
            },
            Config {
                max_users: 4,
                ..cfg(5, 10, 1)
            },
            Config {
                step_size: 0,
                ..cfg(1, 10, 1)
            },
            Config {
                stage_duration_secs: 0,
                ..cfg(1, 10, 1)
            },
            Config {
                max_error_rate: None,
                max_p95_ms: None,
                ..cfg(1, 10, 1)
            },
            // 11 stages of 10 min exceed the 1 h limit.
            Config {
                stage_duration_secs: 600,
                ..cfg(1, 11, 1)
            },
        ];
        for c in bad {
            assert!(!c.validate().is_empty(), "{c:?}");
        }
        assert!(cfg(10, 100, 10).validate().is_empty());
    }

    #[test]
    fn evaluates_thresholds() {
        let c = cfg(1, 10, 1);
        assert_eq!(evaluate(&c, &measured(0.01, 200.0)).0, StageResult::Passed);
        assert_eq!(
            evaluate(&c, &measured(0.02, 10.0)).0,
            StageResult::ThresholdExceeded
        );
        let (result, detail) = evaluate(&c, &measured(0.0, 250.0));
        assert_eq!(result, StageResult::ThresholdExceeded);
        assert!(detail.contains("p95"), "{detail}");
        let only_p95 = Config {
            max_error_rate: None,
            ..c.clone()
        };
        assert_eq!(
            evaluate(&only_p95, &measured(0.9, 10.0)).0,
            StageResult::Passed
        );
    }

    #[test]
    fn unmeasured_stages_fail_instead_of_passing() {
        let c = cfg(1, 10, 1);
        let no_requests = Measured {
            requests: 0,
            ..measured(0.0, 0.0)
        };
        assert_eq!(evaluate(&c, &no_requests).0, StageResult::Failed);
        let logins_failed = Measured {
            users_logged_in: 3,
            ..measured(0.0, 1.0)
        };
        assert_eq!(evaluate(&c, &logins_failed).0, StageResult::Failed);
    }
}
