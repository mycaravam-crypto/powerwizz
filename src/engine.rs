use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Url;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use std::collections::BTreeMap;

use serde_json::json;

use crate::metrics::{Histogram, Metrics};
use crate::model::{
    self, Account, AuthStrategy, Method, OnError, Step, TestDefinition, is_local_host, resolve_url,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorKind {
    /// Response arrived but its status was not expected.
    Status,
    Timeout,
    Connect,
    Other,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Status => "status",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Connect => "connect",
            ErrorKind::Other => "other",
        }
    }
}

/// One executed request. Holds no credentials, cookies, headers, bodies or query strings.
#[derive(Clone, Debug)]
pub struct RequestRecord {
    /// Time since test start when the request began.
    pub at: Duration,
    pub vu: u32,
    /// "scenario/step"
    pub step: String,
    pub method: Method,
    pub path: String,
    pub status: Option<u16>,
    pub duration: Duration,
    pub error: Option<ErrorKind>,
    /// Sanitized, truncated; empty on success.
    pub message: String,
}

/// Placeholder values of one virtual user: {{username}}, {{password}}, {{vu_id}}
/// and whatever its steps extracted from JSON responses (e.g. a JWT).
#[derive(Debug, Default)]
pub struct Vars {
    pub username: String,
    pub password: String,
    pub vu_id: u32,
    /// Written by steps with `extract`; never leaves this user.
    pub extracted: Mutex<Vec<(String, String)>>,
}

pub fn fill(s: &str, v: &Vars) -> String {
    if !s.contains("{{") {
        return s.to_string();
    }
    let mut s = s
        .replace("{{username}}", &v.username)
        .replace("{{password}}", &v.password)
        .replace("{{vu_id}}", &v.vu_id.to_string());
    for (name, value) in v.extracted.lock().unwrap().iter() {
        s = s.replace(&format!("{{{{{name}}}}}"), value);
    }
    s
}

/// Stores the values `step.extract` points at in `body`. Errors name the
/// pointer, never a value.
fn extract(step: &Step, body: &[u8], vars: &Vars) -> Result<(), String> {
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "extract: response is not JSON".to_string())?;
    let mut found = Vec::with_capacity(step.extract.len());
    for (name, pointer) in &step.extract {
        let value = match json.pointer(pointer) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Null) | None => {
                return Err(format!("extract {name}: {pointer} not found"));
            }
            Some(other) => other.to_string(),
        };
        found.push((name.clone(), value));
    }
    let mut ex = vars.extracted.lock().unwrap();
    for (name, value) in found {
        match ex.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => ex.push((name, value)),
        }
    }
    Ok(())
}

/// Bodies read for extraction are capped; tokens are far smaller.
const MAX_EXTRACT_BODY: usize = 1 << 20;

/// One client per virtual user: own cookie jar, own connection pool.
pub fn new_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .timeout(timeout)
        .build()
        .expect("reqwest client")
}

const MAX_MESSAGE: usize = 200;

pub struct StepCtx<'a> {
    pub client: &'a reqwest::Client,
    pub base: &'a Url,
    pub default_headers: &'a [(String, String)],
    pub vars: &'a Vars,
    pub start: Instant,
}

pub async fn run_step(ctx: &StepCtx<'_>, label: &str, step: &Step) -> RequestRecord {
    let begin = Instant::now();
    let mut rec = RequestRecord {
        at: begin - ctx.start,
        vu: ctx.vars.vu_id,
        step: label.to_string(),
        method: step.method,
        path: String::new(),
        status: None,
        duration: Duration::ZERO,
        error: None,
        message: String::new(),
    };
    let url = match resolve_url(ctx.base, &fill(&step.url, ctx.vars)) {
        Ok(u) => u,
        Err(e) => {
            rec.error = Some(ErrorKind::Other);
            rec.message = truncate(format!("invalid URL: {e}"));
            return rec;
        }
    };
    // Path from the unfilled template: never contains filled-in credentials.
    rec.path = resolve_url(ctx.base, &step.url)
        .map(|u| u.path().replace("%7B%7B", "{{").replace("%7D%7D", "}}"))
        .unwrap_or_default();

    let method = reqwest::Method::from_bytes(step.method.as_str().as_bytes()).unwrap();
    let mut req = ctx.client.request(method, url);
    for (k, v) in ctx.default_headers.iter().chain(&step.headers) {
        let v = fill(v, ctx.vars);
        // Not extracted yet (e.g. `Bearer {{token}}` on the login itself): leave it out.
        if model::placeholders(&v).next().is_none() {
            req = req.header(k, v);
        }
    }
    if !step.body.is_empty() {
        req = req.body(fill(&step.body, ctx.vars));
    }

    let keep_body = !step.extract.is_empty();
    let result = async {
        let mut resp = req.send().await?;
        let status = resp.status().as_u16();
        // Read the body so latency covers the full response; only steps that
        // extract values keep it (capped), the rest is discarded chunk by chunk.
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            if keep_body && body.len() < MAX_EXTRACT_BODY {
                body.extend_from_slice(&chunk);
            }
        }
        Ok::<(u16, Vec<u8>), reqwest::Error>((status, body))
    }
    .await;
    rec.duration = begin.elapsed();
    match result {
        Ok((status, body)) => {
            rec.status = Some(status);
            if !step.status_ok(status) {
                rec.error = Some(ErrorKind::Status);
                rec.message = format!("unexpected status {status}");
            } else if keep_body && let Err(e) = extract(step, &body, ctx.vars) {
                rec.error = Some(ErrorKind::Other);
                rec.message = e;
            }
        }
        Err(e) => {
            rec.status = e.status().map(|s| s.as_u16());
            rec.error = Some(if e.is_timeout() {
                ErrorKind::Timeout
            } else if e.is_connect() {
                ErrorKind::Connect
            } else {
                ErrorKind::Other
            });
            // without_url: the URL may carry query tokens or filled-in credentials.
            rec.message = truncate(error_chain(&e.without_url()));
        }
    }
    rec
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_MESSAGE {
        let mut end = MAX_MESSAGE;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Stopping,
    Finished,
}

/// State shared between the engine tasks and the GUI.
pub struct Shared {
    pub def: Arc<TestDefinition>,
    pub start: Instant,
    pub started_at: SystemTime,
    pub duration: Duration,
    pub users: u32,
    pub started: AtomicU32,
    pub active: AtomicU32,
    /// Fully completed scenario iterations.
    pub iterations: AtomicU64,
    // ponytail: one mutex, locked once per batch by the aggregator and briefly by the GUI.
    // Switch to snapshots over a watch channel if GUI reads ever stall the aggregator.
    pub metrics: Mutex<Metrics>,
    status: Mutex<Status>,
    stop: watch::Sender<bool>,
    /// Requests issued so far, for the global request limit.
    issued: AtomicU64,
    request_limit: Option<u64>,
    ended: OnceLock<Duration>,
}

impl Shared {
    pub fn status(&self) -> Status {
        *self.status.lock().unwrap()
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Test runtime: frozen once the test has finished.
    pub fn runtime(&self) -> Duration {
        self.ended.get().copied().unwrap_or_else(|| self.elapsed())
    }

    /// Results as `powerwizz.report/v1` JSON for docwizz. Contains no credentials,
    /// headers, bodies, cookies, error messages or query strings.
    pub fn report_json(&self) -> String {
        let d = &self.def;
        let m = self.metrics.lock().unwrap();
        let runtime = self.runtime().as_secs_f64();
        let ms = |d: Duration| (d.as_secs_f64() * 1e6).round() / 1e3;
        let latency = |h: &Histogram| {
            json!({
                "mean": ms(h.mean()),
                "p50": ms(h.percentile(0.50)),
                "p95": ms(h.percentile(0.95)),
                "p99": ms(h.percentile(0.99)),
            })
        };
        let rate = |f: u64, n: u64| if n == 0 { 0.0 } else { f as f64 / n as f64 };
        let mut target = Url::parse(&d.base_url).expect("validated");
        let _ = target.set_username("");
        let _ = target.set_password(None);
        target.set_query(None);
        target.set_fragment(None);
        let report = json!({
            "schema": "powerwizz.report/v1",
            "generator": format!("powerwizz {}", env!("CARGO_PKG_VERSION")),
            "started_at": iso_utc(self.started_at),
            "runtime_s": (runtime * 10.0).round() / 10.0,
            "target": { "base_url": target.as_str(), "local": is_local_host(&d.base_url) },
            "config": {
                "users": d.users,
                "ramp_up_s": d.ramp_up_secs,
                "duration_s": d.duration_secs,
                "request_limit": d.request_limit,
                "timeout_s": d.timeout_secs,
                "auth": format!("{:?}", d.auth),
                "scenarios": d.scenarios.iter().map(|s| &s.name).collect::<Vec<_>>(),
            },
            "totals": {
                "users_started": self.started.load(Ordering::Relaxed),
                "iterations": self.iterations.load(Ordering::Relaxed),
                "requests": m.requests,
                "failures": m.failures,
                "error_rate": rate(m.failures, m.requests),
                "rps": m.requests as f64 / runtime.max(0.001),
                "latency_ms": latency(&m.latency),
                "status_counts": m.status_counts.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_, _>>(),
                "error_kinds": m.by_kind.iter().map(|(k, v)| (k.as_str(), *v)).collect::<BTreeMap<_, _>>(),
            },
            "steps": m.per_step.iter().map(|(name, s)| json!({
                "name": name,
                "method": s.method,
                "path": s.path,
                "requests": s.requests,
                "failures": s.failures,
                "error_rate": rate(s.failures, s.requests),
                "latency_ms": latency(&s.latency),
            })).collect::<Vec<_>>(),
            "series": m.series.iter().map(|p| json!({
                "t": p.t,
                "requests": p.requests,
                "failures": p.failures,
                "p50_ms": ms(p.p50),
                "p95_ms": ms(p.p95),
                "p99_ms": ms(p.p99),
            })).collect::<Vec<_>>(),
        });
        serde_json::to_string_pretty(&report).expect("serializable")
    }

    fn limit_reached(&self) -> bool {
        self.request_limit
            .is_some_and(|l| self.issued.load(Ordering::Relaxed) >= l)
    }

    fn set_status(&self, s: Status) {
        *self.status.lock().unwrap() = s;
    }
}

pub struct Handle {
    pub shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Handle {
    /// Prevents new virtual users and aborts in-flight requests.
    pub fn stop(&self) {
        self.shared.stop.send_replace(true);
    }

    /// Resolves once every task has ended and all results are aggregated.
    pub async fn wait(&mut self) {
        let _ = (&mut self.task).await;
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` without a date crate (civil-from-days, Howard Hinnant).
fn iso_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// When virtual user `i` of `n` starts: spread evenly over the ramp-up.
pub fn start_offset(i: u32, n: u32, ramp_up: Duration) -> Duration {
    ramp_up.mul_f64(i as f64 / n.max(1) as f64)
}

/// Bounded so a slow aggregator applies backpressure instead of growing memory.
const CHANNEL_CAP: usize = 10_000;

/// Validates and starts a test. Must be called inside a tokio runtime.
pub fn start(
    def: TestDefinition,
    accounts: Vec<Account>,
    keep_records: bool,
) -> Result<Handle, Vec<String>> {
    let mut errs = def.validate();
    if def.auth == AuthStrategy::PerUser && accounts.len() < def.users as usize {
        errs.push(format!(
            "{} accounts imported, {} virtual users configured",
            accounts.len(),
            def.users
        ));
    }
    if !errs.is_empty() {
        return Err(errs);
    }
    let base = Url::parse(&def.base_url).expect("validated");
    let (stop, _) = watch::channel(false);
    let def = Arc::new(def);
    let shared = Arc::new(Shared {
        def: def.clone(),
        start: Instant::now(),
        started_at: SystemTime::now(),
        duration: Duration::from_secs(def.duration_secs),
        users: def.users,
        started: AtomicU32::new(0),
        active: AtomicU32::new(0),
        iterations: AtomicU64::new(0),
        metrics: Mutex::new(Metrics::new(keep_records)),
        status: Mutex::new(Status::Running),
        stop,
        issued: AtomicU64::new(0),
        request_limit: def.request_limit,
        ended: OnceLock::new(),
    });
    let (tx, rx) = mpsc::channel(CHANNEL_CAP);
    let aggregator = tokio::spawn(aggregate(shared.clone(), rx));
    let task = tokio::spawn(control(shared.clone(), def, base, accounts, tx, aggregator));
    Ok(Handle { shared, task })
}

async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

async fn control(
    shared: Arc<Shared>,
    def: Arc<TestDefinition>,
    base: Url,
    accounts: Vec<Account>,
    tx: mpsc::Sender<RequestRecord>,
    aggregator: JoinHandle<()>,
) {
    let mut stop = shared.stop.subscribe();
    let deadline = tokio::time::Instant::from_std(shared.start + shared.duration);
    let ramp_up = Duration::from_secs(def.ramp_up_secs);
    let mut users = JoinSet::new();
    let mut accounts = accounts.into_iter();
    for i in 0..def.users {
        if shared.limit_reached() {
            break;
        }
        let at = tokio::time::Instant::from_std(shared.start + start_offset(i, def.users, ramp_up));
        tokio::select! {
            _ = tokio::time::sleep_until(at) => {}
            _ = tokio::time::sleep_until(deadline) => break,
            _ = stopped(&mut stop) => break,
        }
        let vars = match def.auth {
            AuthStrategy::None => Vars {
                vu_id: i,
                ..Default::default()
            },
            AuthStrategy::Shared => Vars {
                username: def.shared_username.clone(),
                password: def.shared_password.clone(),
                vu_id: i,
                ..Default::default()
            },
            AuthStrategy::PerUser => {
                let a = accounts.next().expect("validated");
                Vars {
                    username: a.username,
                    password: a.password,
                    vu_id: i,
                    ..Default::default()
                }
            }
        };
        shared.started.fetch_add(1, Ordering::Relaxed);
        users.spawn(virtual_user(
            shared.clone(),
            def.clone(),
            base.clone(),
            vars,
            tx.clone(),
        ));
    }
    // Users end on their own when the request limit is reached or login fails.
    let all_done = async { while users.join_next().await.is_some() {} };
    tokio::select! {
        _ = tokio::time::sleep_until(deadline) => {}
        _ = stopped(&mut stop) => {}
        _ = all_done => {}
    }
    shared.set_status(Status::Stopping);
    shared.stop.send_replace(true);
    users.join_all().await;
    drop(tx);
    let _ = aggregator.await;
    let _ = shared.ended.set(shared.elapsed());
    shared.set_status(Status::Finished);
}

async fn virtual_user(
    shared: Arc<Shared>,
    def: Arc<TestDefinition>,
    base: Url,
    vars: Vars,
    tx: mpsc::Sender<RequestRecord>,
) {
    shared.active.fetch_add(1, Ordering::Relaxed);
    let client = new_client(Duration::from_secs(def.timeout_secs));
    let ctx = StepCtx {
        client: &client,
        base: &base,
        default_headers: &def.headers,
        vars: &vars,
        start: shared.start,
    };
    let mut stop = shared.stop.subscribe();
    // The future is dropped on stop, which aborts any in-flight request or pause.
    tokio::select! {
        _ = user_loop(&shared, &def, &ctx, &tx) => {}
        _ = stopped(&mut stop) => {}
    }
    shared.active.fetch_sub(1, Ordering::Relaxed);
}

/// Runs the login steps; false if one failed or the request limit was reached.
async fn login(
    shared: &Shared,
    def: &TestDefinition,
    ctx: &StepCtx<'_>,
    tx: &mpsc::Sender<RequestRecord>,
) -> bool {
    for step in &def.login_steps {
        if exec(shared, ctx, "login", step, tx).await != Some(Outcome::Ok) {
            return false;
        }
    }
    true
}

async fn user_loop(
    shared: &Shared,
    def: &TestDefinition,
    ctx: &StepCtx<'_>,
    tx: &mpsc::Sender<RequestRecord>,
) {
    if !login(shared, def, ctx, tx).await {
        return; // login failed or limit reached: this user ends
    }
    // Set after a re-login until a step succeeds, so a 401 that a fresh login
    // doesn't fix can't turn into a login storm.
    let mut relogged = false;
    loop {
        let sc = &def.scenarios[fastrand::usize(..def.scenarios.len())];
        let mut completed = true;
        for step in &sc.steps {
            match exec(shared, ctx, &sc.name, step, tx).await {
                None => return,
                Some(Outcome::Ok) => relogged = false,
                // Session or token expired: log in again, continue with the next iteration.
                Some(Outcome::Unauthorized) if !def.login_steps.is_empty() && !relogged => {
                    if !login(shared, def, ctx, tx).await {
                        return;
                    }
                    relogged = true;
                    completed = false;
                    break;
                }
                Some(_) if def.on_error == OnError::StopUser => return,
                Some(_) => {
                    completed = false;
                    break;
                }
            }
        }
        if completed {
            shared.iterations.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    /// 401 although 401 was not expected.
    Unauthorized,
    Failed,
}

/// Runs one step and its pause. `None` when the request limit is reached.
async fn exec(
    shared: &Shared,
    ctx: &StepCtx<'_>,
    scenario: &str,
    step: &Step,
    tx: &mpsc::Sender<RequestRecord>,
) -> Option<Outcome> {
    // In-flight requests of other users still complete, so exactly `limit` are recorded.
    if let Some(limit) = shared.request_limit
        && shared.issued.fetch_add(1, Ordering::Relaxed) >= limit
    {
        return None;
    }
    let rec = run_step(ctx, &format!("{scenario}/{}", step.name), step).await;
    let outcome = match (rec.error, rec.status) {
        (None, _) => Outcome::Ok,
        (Some(ErrorKind::Status), Some(401)) => Outcome::Unauthorized,
        _ => Outcome::Failed,
    };
    if tx.send(rec).await.is_err() {
        return None;
    }
    if step.pause_max_ms > 0 {
        let ms = fastrand::u64(step.pause_min_ms..=step.pause_max_ms);
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    Some(outcome)
}

async fn aggregate(shared: Arc<Shared>, mut rx: mpsc::Receiver<RequestRecord>) {
    let mut buf = Vec::with_capacity(1024);
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            n = rx.recv_many(&mut buf, 1024) => {
                if n == 0 {
                    break;
                }
                let mut m = shared.metrics.lock().unwrap();
                for rec in buf.drain(..) {
                    m.add(rec);
                }
            }
            _ = tick.tick() => shared.metrics.lock().unwrap().tick(shared.elapsed().as_secs()),
        }
    }
    let mut m = shared.metrics.lock().unwrap();
    let end = shared.elapsed().min(shared.duration).as_secs();
    m.tick(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_only_known_placeholders() {
        let v = Vars {
            username: "bob".into(),
            password: "pw".into(),
            vu_id: 7,
            extracted: Mutex::new(vec![("token".into(), "abc".into())]),
        };
        assert_eq!(
            fill("u={{username}}&p={{password}}&id={{vu_id}}&x={{other}}", &v),
            "u=bob&p=pw&id=7&x={{other}}"
        );
        assert_eq!(fill("plain", &v), "plain");
    }

    #[test]
    fn start_offsets_spread_over_ramp_up() {
        let r = Duration::from_secs(10);
        assert_eq!(start_offset(0, 5, r), Duration::ZERO);
        assert_eq!(start_offset(1, 5, r), Duration::from_secs(2));
        assert_eq!(start_offset(4, 5, r), Duration::from_secs(8));
        assert_eq!(start_offset(3, 5, Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn formats_utc_timestamps() {
        let at = |s| UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(iso_utc(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(at(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(at(1_791_913_565)), "2026-10-13T17:46:05Z");
    }

    #[test]
    fn fills_extracted_values() {
        let v = Vars {
            extracted: Mutex::new(vec![("token".into(), "abc".into())]),
            ..Default::default()
        };
        assert_eq!(fill("Bearer {{token}}", &v), "Bearer abc");
    }

    #[test]
    fn extracts_strings_numbers_and_reports_missing() {
        let step = Step {
            extract: vec![
                ("token".into(), "/auth/token".into()),
                ("id".into(), "/id".into()),
            ],
            ..Default::default()
        };
        let v = Vars::default();
        extract(&step, br#"{"auth":{"token":"t1"},"id":42}"#, &v).unwrap();
        assert_eq!(fill("{{token}}/{{id}}", &v), "t1/42");
        // A later extraction overwrites, e.g. after a re-login.
        extract(&step, br#"{"auth":{"token":"t2"},"id":42}"#, &v).unwrap();
        assert_eq!(fill("{{token}}", &v), "t2");
        let e = extract(&step, br#"{"auth":{},"id":"secret-id"}"#, &v).unwrap_err();
        assert_eq!(e, "extract token: /auth/token not found");
        assert!(
            extract(&step, b"<html>", &v)
                .unwrap_err()
                .contains("not JSON")
        );
        assert_eq!(
            fill("{{token}}", &v),
            "t2",
            "failed extraction keeps old values"
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        let s = truncate("ä".repeat(150));
        assert!(s.len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(s.ends_with('…'));
    }
}
