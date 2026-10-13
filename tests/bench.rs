//! Load benchmark against the in-process test server. Not part of `cargo test`:
//!
//! ```sh
//! BENCH_USERS=200 cargo test --release --test bench -- --ignored --nocapture
//! ```
//!
//! Engine and test server share one process, so CPU/RAM numbers include both.
mod common;

use std::time::Duration;

use powerwizz::engine::start;
use powerwizz::model::{Scenario, Step, TestDefinition};

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench() {
    let users: u32 = std::env::var("BENCH_USERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let secs: u64 = std::env::var("BENCH_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let srv = common::start().await;
    let def = TestDefinition {
        base_url: srv.url("/"),
        users,
        ramp_up_secs: 2,
        duration_secs: secs,
        scenarios: vec![Scenario {
            name: "s".into(),
            steps: vec![Step::default()],
        }],
        ..Default::default()
    };
    let mut h = start(def, vec![], false).unwrap();
    h.wait().await;
    let m = h.shared.metrics.lock().unwrap();
    let rt = h.shared.runtime().as_secs_f64();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "users={users} runtime={rt:.1}s requests={} rps_avg={:.0} failures={} p50={:.2}ms p95={:.2}ms p99={:.2}ms",
        m.requests,
        m.requests as f64 / rt,
        m.failures,
        ms(m.latency.percentile(0.5)),
        ms(m.latency.percentile(0.95)),
        ms(m.latency.percentile(0.99)),
    );
}
