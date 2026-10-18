//! Capacity discovery against the in-process test server: stages, thresholds, cancellation and failures.
mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use powerwizz::capacity::{self, Config, Outcome, StageResult, StopReason};
use powerwizz::model::{Account, AuthStrategy, Scenario, Step, TestDefinition};
use tokio::sync::watch;

fn def(srv: &common::TestServer, url: &str) -> TestDefinition {
    TestDefinition {
        base_url: srv.url("/"),
        ramp_up_secs: 0,
        timeout_secs: 5,
        scenarios: vec![Scenario {
            name: "s".into(),
            steps: vec![Step {
                url: url.into(),
                pause_min_ms: 20,
                pause_max_ms: 50,
                ..Default::default()
            }],
        }],
        ..Default::default()
    }
}

fn cfg(start: u32, max: u32, step: u32) -> Config {
    Config {
        start_users: start,
        max_users: max,
        step_size: step,
        stage_duration_secs: 1,
        max_error_rate: Some(0.5),
        max_p95_ms: None,
    }
}

async fn run(
    d: TestDefinition,
    accounts: Vec<Account>,
    c: &Config,
) -> Result<Outcome, Vec<String>> {
    let (_keep_open, cancel) = watch::channel(false);
    capacity::run(d, accounts, false, c, cancel, |_| {}).await
}

fn users_and_results(o: &Outcome) -> Vec<(u32, StageResult)> {
    o.stages.iter().map(|s| (s.users, s.result)).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn passing_stages_climb_to_max_users_and_never_beyond() {
    let srv = common::start().await;
    let o = run(def(&srv, "/vu/{{vu_id}}"), vec![], &cfg(1, 4, 2))
        .await
        .unwrap();

    assert_eq!(
        users_and_results(&o),
        [
            (1, StageResult::Passed),
            (3, StageResult::Passed),
            (4, StageResult::Passed)
        ]
    );
    assert_eq!(o.stop_reason, StopReason::MaxUsersReached);
    assert_eq!(o.highest_passed_users(), Some(4));
    for s in &o.stages {
        assert_eq!(
            s.measured.users_started, s.users,
            "stage ran with its own user count"
        );
        assert!(s.measured.requests > 0 && s.measured.rps > 0.0);
    }
    let log = srv.log.lock().unwrap();
    assert!(
        !log.iter().any(|(_, p)| p == "/vu/4"),
        "a stage exceeded max_users"
    );
    assert!(
        log.iter().any(|(_, p)| p == "/vu/3"),
        "the last stage ran 4 users"
    );
    assert_eq!(o.last.as_ref().unwrap().users, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn error_rate_violation_stops_escalation() {
    let srv = common::start().await;
    let o = run(def(&srv, "/status/500"), vec![], &cfg(2, 10, 2))
        .await
        .unwrap();

    assert_eq!(users_and_results(&o), [(2, StageResult::ThresholdExceeded)]);
    assert_eq!(o.stop_reason, StopReason::ThresholdExceeded);
    assert_eq!(o.highest_passed_users(), None);
    assert!(
        o.stages[0].detail.contains("error rate"),
        "{}",
        o.stages[0].detail
    );
    let hits = srv.hits();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(srv.hits(), hits, "load continued after the violation");
}

#[tokio::test(flavor = "multi_thread")]
async fn p95_violation_stops_escalation() {
    let srv = common::start().await;
    let c = Config {
        max_error_rate: None,
        max_p95_ms: Some(20.0),
        ..cfg(1, 5, 1)
    };
    let o = run(def(&srv, "/delay/100"), vec![], &c).await.unwrap();

    assert_eq!(users_and_results(&o), [(1, StageResult::ThresholdExceeded)]);
    assert!(o.stages[0].measured.p95_ms > 20.0);
    assert!(o.stages[0].detail.contains("p95"), "{}", o.stages[0].detail);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_stops_the_running_stage_and_starts_no_other() {
    let srv = common::start().await;
    let c = Config {
        stage_duration_secs: 30,
        ..cfg(1, 3, 1)
    };
    let (cancel, cancelled) = watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel.send_replace(true);
        // Keep the sender alive: a dropped sender must not look like a cancellation.
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let t0 = Instant::now();
    let o = capacity::run(def(&srv, "/"), vec![], false, &c, cancelled, |_| {})
        .await
        .unwrap();

    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "took {:?}",
        t0.elapsed()
    );
    assert_eq!(users_and_results(&o), [(1, StageResult::Cancelled)]);
    assert_eq!(o.stop_reason, StopReason::Cancelled);
    assert_eq!(
        o.highest_passed_users(),
        None,
        "a cancelled stage never counts as passed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_before_the_first_stage_runs_nothing() {
    let srv = common::start().await;
    let (cancel, cancelled) = watch::channel(true);
    let o = capacity::run(
        def(&srv, "/"),
        vec![],
        false,
        &cfg(1, 3, 1),
        cancelled,
        |_| {},
    )
    .await
    .unwrap();
    drop(cancel);

    assert!(o.stages.is_empty() && o.last.is_none());
    assert_eq!(o.stop_reason, StopReason::Cancelled);
    assert_eq!(srv.hits(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_logins_fail_the_stage_instead_of_passing() {
    let srv = common::start().await;
    let mut d = def(&srv, "/");
    d.auth = AuthStrategy::Shared;
    d.shared_username = "u".into();
    // An empty body makes /login answer 401: no user gets past the login.
    d.login_steps = vec![Step {
        url: "/login".into(),
        method: powerwizz::model::Method::Post,
        ..Default::default()
    }];
    let o = run(d, vec![], &cfg(2, 4, 2)).await.unwrap();

    assert_eq!(users_and_results(&o), [(2, StageResult::Failed)]);
    assert_eq!(o.stop_reason, StopReason::Failed);
    assert!(
        o.stages[0].detail.contains("logged in"),
        "{}",
        o.stages[0].detail
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_settings_are_refused_before_any_request() {
    let srv = common::start().await;
    let no_threshold = Config {
        max_error_rate: None,
        ..cfg(1, 2, 1)
    };
    assert!(run(def(&srv, "/"), vec![], &no_threshold).await.is_err());

    // Accounts per user must cover the largest stage, not just the first.
    let mut d = def(&srv, "/");
    d.auth = AuthStrategy::PerUser;
    d.login_steps = vec![Step {
        url: "/login".into(),
        ..Default::default()
    }];
    let accounts = vec![Account::default(); 2];
    let errs = run(d, accounts, &cfg(2, 4, 2)).await.err().unwrap();
    assert!(
        errs.iter().any(|e| e.contains("accounts imported")),
        "{errs:?}"
    );

    // Ramp-up longer than a stage would never reach the stage's load.
    let mut d = def(&srv, "/");
    d.ramp_up_secs = 5;
    assert!(run(d, vec![], &cfg(1, 2, 1)).await.is_err());

    assert_eq!(srv.hits(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn report_section_lists_every_stage() {
    let srv = common::start().await;
    let c = cfg(1, 2, 1);
    let o = run(def(&srv, "/"), vec![], &c).await.unwrap();
    let j = o.to_json(&c);

    assert_eq!(j["config"]["start_users"], 1);
    assert_eq!(j["config"]["max_users"], 2);
    assert_eq!(j["config"]["step_size"], 1);
    assert_eq!(j["config"]["stage_duration_s"], 1);
    assert_eq!(j["config"]["max_error_rate"], 0.5);
    assert!(j["config"]["max_p95_latency_ms"].is_null());
    assert_eq!(j["highest_passed_users"], 2);
    assert_eq!(j["stop_reason"], "max_users_reached");
    let stages = j["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    for (s, users) in stages.iter().zip([1, 2]) {
        assert_eq!(s["users"], users);
        assert_eq!(s["result"], "passed");
        for key in ["requests", "rps", "p95_ms", "error_rate"] {
            assert!(s[key].is_number(), "{key} in {s}");
        }
    }
    assert_eq!(
        o.last.unwrap().started.load(Ordering::Relaxed),
        2,
        "the regular report describes the last stage"
    );
}
