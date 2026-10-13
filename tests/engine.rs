mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use powerwizz::engine::{ErrorKind, Status, start};
use powerwizz::model::{Account, AuthStrategy, Method, Scenario, Step, TestDefinition};

fn def(srv: &common::TestServer, url: &str) -> TestDefinition {
    TestDefinition {
        base_url: srv.url("/"),
        users: 5,
        ramp_up_secs: 0,
        duration_secs: 2,
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

#[tokio::test(flavor = "multi_thread")]
async fn ramp_up_spreads_user_starts() {
    let srv = common::start().await;
    let mut d = def(&srv, "/vu/{{vu_id}}");
    d.users = 4;
    d.ramp_up_secs = 2;
    d.duration_secs = 3;
    let mut h = start(d, vec![], false).unwrap();
    h.wait().await;

    let log = srv.log.lock().unwrap();
    for i in 0..4u64 {
        let first = log
            .iter()
            .find(|(_, p)| *p == format!("/vu/{i}"))
            .unwrap()
            .0;
        let expected = Duration::from_millis(500 * i);
        assert!(
            first + Duration::from_millis(50) >= expected,
            "vu {i} started at {first:?}"
        );
        assert!(
            first < expected + Duration::from_millis(400),
            "vu {i} started at {first:?}"
        );
    }
    assert!(
        !log.iter().any(|(_, p)| p == "/vu/4"),
        "more users than configured"
    );
    assert_eq!(h.shared.started.load(Ordering::Relaxed), 4);
    assert_eq!(h.shared.active.load(Ordering::Relaxed), 0);
    assert_eq!(h.shared.status(), Status::Finished);
    assert!(h.shared.iterations.load(Ordering::Relaxed) > 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_test_and_prevents_new_users() {
    let srv = common::start().await;
    let mut d = def(&srv, "/");
    d.users = 50;
    d.ramp_up_secs = 10;
    d.duration_secs = 60;
    let mut h = start(d, vec![], false).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t = Instant::now();
    h.stop();
    h.wait().await;
    assert!(t.elapsed() < Duration::from_secs(1));
    let started = h.shared.started.load(Ordering::Relaxed);
    assert!((1..10).contains(&started), "started {started}");
    // A request aborted mid-flight may still reach the server just after stop.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let hits = srv.hits();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(srv.hits(), hits, "requests after stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn timeouts_counted_and_test_ends_on_time() {
    let srv = common::start().await;
    let mut d = def(&srv, "/delay/5000");
    d.timeout_secs = 1;
    d.duration_secs = 2;
    let t = Instant::now();
    let mut h = start(d, vec![], false).unwrap();
    h.wait().await;
    assert!(
        t.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t.elapsed()
    );
    let m = h.shared.metrics.lock().unwrap();
    assert!(m.kind_count(ErrorKind::Timeout) >= 5);
    assert_eq!(m.requests, m.failures);
}

#[tokio::test(flavor = "multi_thread")]
async fn request_limit_ends_test() {
    let srv = common::start().await;
    let mut d = def(&srv, "/");
    d.duration_secs = 60;
    d.request_limit = Some(20);
    let t = Instant::now();
    let mut h = start(d, vec![], false).unwrap();
    h.wait().await;
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(srv.hits(), 20);
    assert_eq!(h.shared.metrics.lock().unwrap().requests, 20);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_invalid_definition() {
    let srv = common::start().await;
    let mut d = def(&srv, "/");
    d.users = 0;
    assert!(start(d, vec![], false).is_err());
}

fn login_def(srv: &common::TestServer) -> TestDefinition {
    let mut d = def(srv, "/cookie/{{username}}:{{password}}");
    d.users = 3;
    d.duration_secs = 1;
    d.login_steps = vec![Step {
        name: "login".into(),
        method: Method::Post,
        url: "/login".into(),
        body: "{{username}}:{{password}}".into(),
        ..Default::default()
    }];
    d
}

#[tokio::test(flavor = "multi_thread")]
async fn per_user_accounts_log_in_separately() {
    let srv = common::start().await;
    let mut d = login_def(&srv);
    d.auth = AuthStrategy::PerUser;
    let accounts: Vec<Account> = (0..3)
        .map(|i| Account {
            username: format!("user{i}"),
            password: format!("pw{i}"),
        })
        .collect();
    assert!(start(d.clone(), accounts[..2].to_vec(), false).is_err());

    let mut h = start(d, accounts, true).unwrap();
    h.wait().await;
    let mut bodies = srv.bodies.lock().unwrap().clone();
    bodies.sort();
    assert_eq!(bodies, ["user0:pw0", "user1:pw1", "user2:pw2"]);
    let m = h.shared.metrics.lock().unwrap();
    assert!(m.requests > 3);
    assert_eq!(m.failures, 0, "{:?}", m.errors);
    assert!(
        m.records
            .iter()
            .all(|r| ["/login", "/cookie/{{username}}:{{password}}"].contains(&r.path.as_str()))
    );
    let csv = powerwizz::metrics::records_csv(&m);
    assert!(!csv.contains("pw0") && !csv.contains("user0"), "{csv}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_account_and_failed_login() {
    let srv = common::start().await;
    let mut d = login_def(&srv);
    d.auth = AuthStrategy::Shared;
    d.shared_username = "team".into();
    d.shared_password = "secret".into();
    let mut h = start(d.clone(), vec![], false).unwrap();
    h.wait().await;
    assert_eq!(srv.bodies.lock().unwrap().len(), 3);
    assert_eq!(h.shared.metrics.lock().unwrap().failures, 0);

    // An empty login body is rejected: each user records one failure and stops.
    d.login_steps[0].body.clear();
    let mut h = start(d, vec![], false).unwrap();
    h.wait().await;
    let m = h.shared.metrics.lock().unwrap();
    assert_eq!((m.requests, m.failures), (3, 3));
    assert!(m.errors.iter().all(|e| !e.message.contains("secret")));
}
