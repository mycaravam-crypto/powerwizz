//! End-to-end tests against the mock web shop in `examples/mockapp`, driven by
//! its sample test definition and accounts.
#[path = "../examples/mockapp/app.rs"]
mod app;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use powerwizz::engine::{ErrorKind, StepCtx, Vars, new_client, run_step, start};
use powerwizz::model::{self, AuthStrategy, Method, Step, TestDefinition};
use reqwest::Url;

async fn serve(cfg: app::Config) -> (String, Arc<app::Stats>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    (base, app::spawn(l, cfg))
}

fn fast() -> app::Config {
    app::Config {
        latency_ms: (1, 5),
        slow_ms: (10, 20),
        error_rate: 0.0,
        rate_limit: 0,
    }
}

/// The shipped sample, shortened: 20 users, 3 s, short pauses.
fn sample(base: &str) -> TestDefinition {
    let mut d = TestDefinition::load("examples/mockapp/test.json").unwrap();
    d.base_url = base.into();
    (d.users, d.ramp_up_secs, d.duration_secs) = (20, 1, 3);
    for s in d
        .login_steps
        .iter_mut()
        .chain(d.scenarios.iter_mut().flat_map(|s| &mut s.steps))
    {
        (s.pause_min_ms, s.pause_max_ms) = (10, 50);
    }
    d
}

#[tokio::test(flavor = "multi_thread")]
async fn sample_scenario_runs_clean_with_per_user_accounts() {
    let (base, stats) = serve(fast()).await;
    let d = sample(&base);
    assert!(d.validate().is_empty(), "{:?}", d.validate());
    let accounts = model::load_accounts("examples/mockapp/accounts.csv").unwrap();
    assert_eq!(accounts.len(), 500);

    let mut h = start(d, accounts, false).unwrap();
    h.wait().await;
    let m = h.shared.metrics.lock().unwrap();
    assert_eq!(m.failures, 0, "{:?}", m.errors);
    assert_eq!(stats.logins.load(Ordering::Relaxed), 20);
    assert!(stats.orders.load(Ordering::Relaxed) > 0);
    for step in [
        "login/login",
        "browse/list",
        "browse/detail",
        "order/detail",
        "order/order",
    ] {
        assert!(
            m.per_step.contains_key(step),
            "missing {step}: {:?}",
            m.per_step.keys()
        );
    }
    assert_eq!(m.per_step["order/order"].path, "/api/orders");
    assert_eq!(m.per_step["browse/detail"].path, "/api/items/{{vu_id}}");
    assert_eq!(
        m.status_counts.get(&201).copied(),
        Some(m.per_step["order/order"].requests)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_login_the_api_answers_401() {
    let (base, _) = serve(fast()).await;
    let mut d = sample(&base);
    d.auth = AuthStrategy::None;
    d.login_steps.clear();
    let mut h = start(d, vec![], false).unwrap();
    h.wait().await;
    let m = h.shared.metrics.lock().unwrap();
    assert!(m.requests > 0);
    assert_eq!(m.status_counts.get(&401).copied(), Some(m.requests));
    assert_eq!(m.kind_count(ErrorKind::Status), m.requests);
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_and_injected_errors_are_counted() {
    let (base, _) = serve(app::Config {
        rate_limit: 5,
        error_rate: 0.5,
        ..fast()
    })
    .await;
    let mut d = sample(&base);
    for s in d.scenarios.iter_mut().flat_map(|s| &mut s.steps) {
        (s.pause_min_ms, s.pause_max_ms) = (0, 0);
    }
    let accounts = model::load_accounts("examples/mockapp/accounts.csv").unwrap();
    let mut h = start(d, accounts, false).unwrap();
    h.wait().await;
    let m = h.shared.metrics.lock().unwrap();
    assert!(
        m.status_counts.get(&429).copied().unwrap_or(0) > 0,
        "{:?}",
        m.status_counts
    );
    assert!(
        m.status_counts.get(&500).copied().unwrap_or(0) > 0,
        "{:?}",
        m.status_counts
    );
}

/// Documents the v1 limitation: powerwizz can't carry a CSRF token from one
/// response into the next request, so a CSRF-protected form login fails.
#[tokio::test]
async fn csrf_form_login_is_rejected_without_token() {
    let (base, stats) = serve(fast()).await;
    let base = Url::parse(&base).unwrap();
    let client = new_client(Duration::from_secs(5));
    let vars = Vars {
        username: "user0".into(),
        password: "pw0".into(),
        vu_id: 0,
    };
    let ctx = StepCtx {
        client: &client,
        base: &base,
        default_headers: &[],
        vars: &vars,
        start: Instant::now(),
    };
    let page = run_step(
        &ctx,
        "form",
        &Step {
            url: "/login".into(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(page.status, Some(200));
    let post = Step {
        method: Method::Post,
        url: "/login".into(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: "username={{username}}&password={{password}}".into(),
        expected_status: vec![303],
        ..Default::default()
    };
    let r = run_step(&ctx, "form", &post).await;
    assert_eq!((r.status, r.error), (Some(403), Some(ErrorKind::Status)));
    assert_eq!(stats.logins.load(Ordering::Relaxed), 0);
}

/// Realistic benchmark: the sample scenario with its real pauses and the default
/// mock behaviour (5–40 ms latency, 1 % order errors, 20 req/s per session).
///
/// ```sh
/// BENCH_USERS=500 BENCH_SECS=60 cargo test --release --test mockapp -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_sample() {
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (base, stats) = serve(app::Config::default()).await;
    let mut d = TestDefinition::load("examples/mockapp/test.json").unwrap();
    d.base_url = base;
    d.users = env("BENCH_USERS", 50) as u32;
    d.duration_secs = env("BENCH_SECS", 60);
    let accounts = model::load_accounts("examples/mockapp/accounts.csv").unwrap();
    let mut h = start(d, accounts, false).unwrap();
    h.wait().await;
    let m = h.shared.metrics.lock().unwrap();
    let rt = h.shared.runtime().as_secs_f64();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "users={} runtime={rt:.1}s requests={} rps_avg={:.1} failures={} p50={:.1}ms p95={:.1}ms p99={:.1}ms orders={} statuses={:?}",
        h.shared.users,
        m.requests,
        m.requests as f64 / rt,
        m.failures,
        ms(m.latency.percentile(0.5)),
        ms(m.latency.percentile(0.95)),
        ms(m.latency.percentile(0.99)),
        stats.orders.load(Ordering::Relaxed),
        m.status_counts,
    );
}
