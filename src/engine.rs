use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::Url;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::metrics::Metrics;
use crate::model::{Account, AuthStrategy, Method, OnError, Step, TestDefinition, resolve_url};

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

/// Values for the only supported placeholders: {{username}}, {{password}}, {{vu_id}}.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    pub username: String,
    pub password: String,
    pub vu_id: u32,
}

pub fn fill(s: &str, v: &Vars) -> String {
    if !s.contains("{{") {
        return s.to_string();
    }
    s.replace("{{username}}", &v.username)
        .replace("{{password}}", &v.password)
        .replace("{{vu_id}}", &v.vu_id.to_string())
}

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
    rec.path = url.path().to_string();

    let method = reqwest::Method::from_bytes(step.method.as_str().as_bytes()).unwrap();
    let mut req = ctx.client.request(method, url);
    for (k, v) in ctx.default_headers.iter().chain(&step.headers) {
        req = req.header(k, fill(v, ctx.vars));
    }
    if !step.body.is_empty() {
        req = req.body(fill(&step.body, ctx.vars));
    }

    let result = async {
        let mut resp = req.send().await?;
        let status = resp.status().as_u16();
        // Drain the body so latency covers the full response without buffering it.
        while resp.chunk().await?.is_some() {}
        Ok::<u16, reqwest::Error>(status)
    }
    .await;
    rec.duration = begin.elapsed();
    match result {
        Ok(status) => {
            rec.status = Some(status);
            if !step.status_ok(status) {
                rec.error = Some(ErrorKind::Status);
                rec.message = format!("unexpected status {status}");
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
    pub start: Instant,
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
    let shared = Arc::new(Shared {
        start: Instant::now(),
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
    let task = tokio::spawn(control(
        shared.clone(),
        Arc::new(def),
        base,
        accounts,
        tx,
        aggregator,
    ));
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
            },
            AuthStrategy::PerUser => {
                let a = accounts.next().expect("validated");
                Vars {
                    username: a.username,
                    password: a.password,
                    vu_id: i,
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

async fn user_loop(
    shared: &Shared,
    def: &TestDefinition,
    ctx: &StepCtx<'_>,
    tx: &mpsc::Sender<RequestRecord>,
) {
    for step in &def.login_steps {
        match exec(shared, ctx, "login", step, tx).await {
            Some(true) => {}
            _ => return, // login failed or limit reached: this user ends
        }
    }
    loop {
        let sc = &def.scenarios[fastrand::usize(..def.scenarios.len())];
        let mut completed = true;
        for step in &sc.steps {
            match exec(shared, ctx, &sc.name, step, tx).await {
                None => return,
                Some(true) => {}
                Some(false) if def.on_error == OnError::StopUser => return,
                Some(false) => {
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

/// Runs one step and its pause. `None` when the request limit is reached,
/// otherwise whether the step succeeded.
async fn exec(
    shared: &Shared,
    ctx: &StepCtx<'_>,
    scenario: &str,
    step: &Step,
    tx: &mpsc::Sender<RequestRecord>,
) -> Option<bool> {
    // In-flight requests of other users still complete, so exactly `limit` are recorded.
    if let Some(limit) = shared.request_limit
        && shared.issued.fetch_add(1, Ordering::Relaxed) >= limit
    {
        return None;
    }
    let rec = run_step(ctx, &format!("{scenario}/{}", step.name), step).await;
    let ok = rec.error.is_none();
    if tx.send(rec).await.is_err() {
        return None;
    }
    if step.pause_max_ms > 0 {
        let ms = fastrand::u64(step.pause_min_ms..=step.pause_max_ms);
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    Some(ok)
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
    fn truncates_on_char_boundary() {
        let s = truncate("ä".repeat(150));
        assert!(s.len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(s.ends_with('…'));
    }
}
