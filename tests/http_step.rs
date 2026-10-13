mod common;

use std::time::{Duration, Instant};

use powerwizz::engine::{ErrorKind, StepCtx, Vars, new_client, run_step};
use powerwizz::model::{Method, Step};
use reqwest::Url;

fn get(url: &str) -> Step {
    Step {
        url: url.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn cookies_are_isolated_per_user() {
    let srv = common::start().await;
    let base = Url::parse(&srv.url("/")).unwrap();
    let (a, b) = (
        new_client(Duration::from_secs(5)),
        new_client(Duration::from_secs(5)),
    );
    let va = Vars {
        vu_id: 0,
        ..Default::default()
    };
    let vb = Vars {
        vu_id: 1,
        ..Default::default()
    };
    let ctx = |client, vars| StepCtx {
        client,
        base: &base,
        default_headers: &[],
        vars,
        start: Instant::now(),
    };
    let (ca, cb) = (ctx(&a, &va), ctx(&b, &vb));

    assert_eq!(run_step(&ca, "s", &get("/set-cookie/a")).await.error, None);
    assert_eq!(run_step(&cb, "s", &get("/set-cookie/b")).await.error, None);
    assert_eq!(
        run_step(&ca, "s", &get("/cookie/a")).await.status,
        Some(200)
    );
    assert_eq!(
        run_step(&cb, "s", &get("/cookie/b")).await.status,
        Some(200)
    );
    let cross = run_step(&ca, "s", &get("/cookie/b")).await;
    assert_eq!(
        (cross.status, cross.error),
        (Some(403), Some(ErrorKind::Status))
    );
}

#[tokio::test]
async fn classifies_errors_and_sanitizes() {
    let srv = common::start().await;
    let base = Url::parse(&srv.url("/")).unwrap();
    let client = new_client(Duration::from_millis(200));
    let vars = Vars {
        password: "hunter2".into(),
        ..Default::default()
    };
    let ctx = StepCtx {
        client: &client,
        base: &base,
        default_headers: &[],
        vars: &vars,
        start: Instant::now(),
    };

    let ok = run_step(&ctx, "s", &get("/x?token=secret")).await;
    assert_eq!(
        (ok.status, ok.error, ok.path.as_str()),
        (Some(200), None, "/x")
    );

    let mut post = get("/status/201");
    post.method = Method::Post;
    post.body = "{\"p\":\"{{password}}\"}".into();
    assert_eq!(run_step(&ctx, "s", &post).await.error, None);

    let bad = run_step(&ctx, "s", &get("/status/500")).await;
    assert_eq!(bad.error, Some(ErrorKind::Status));

    let slow = run_step(&ctx, "s", &get("/delay/2000?pw={{password}}")).await;
    assert_eq!(slow.error, Some(ErrorKind::Timeout));
    assert!(slow.duration < Duration::from_secs(1));
    assert!(
        !slow.message.contains("hunter2") && !slow.message.contains("pw="),
        "{}",
        slow.message
    );

    let dropped = run_step(&ctx, "s", &get("/drop")).await;
    assert!(dropped.error.is_some());

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let closed = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    // Windows retries SYNs to a closed port for ~2 s before refusing.
    let patient = new_client(Duration::from_secs(10));
    let ctx = StepCtx {
        client: &patient,
        base: &closed,
        ..ctx
    };
    assert_eq!(
        run_step(&ctx, "s", &get("/")).await.error,
        Some(ErrorKind::Connect)
    );
}
