//! `powerwizz-cli` end to end: the real binary against the mock shop, as CI and gitwizz gates run it.
#[path = "../examples/mockapp/app.rs"]
mod app;

use std::path::PathBuf;
use std::process::{Command, Output};

use powerwizz::model::TestDefinition;

async fn serve() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    let cfg = app::Config {
        latency_ms: (1, 5),
        slow_ms: (10, 20),
        error_rate: 0.0,
        rate_limit: 0,
        token_ttl_s: 300,
    };
    app::spawn(l, cfg);
    base
}

/// A fresh directory with the shipped sample, shortened to 10 users and 2 s.
fn workdir(name: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("powerwizz-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut d = TestDefinition::load("examples/mockapp/test.json").unwrap();
    (d.users, d.ramp_up_secs, d.duration_secs) = (10, 1, 2);
    for s in d
        .login_steps
        .iter_mut()
        .chain(d.scenarios.iter_mut().flat_map(|s| &mut s.steps))
    {
        (s.pause_min_ms, s.pause_max_ms) = (10, 50);
    }
    let file = dir.join("test.json");
    d.save(file.to_str().unwrap()).unwrap();
    (dir, file.to_str().unwrap().to_string())
}

/// Runs the binary off the runtime, so the in-process mock keeps serving.
async fn cli(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_powerwizz-cli"))
            .args(args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn args(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| x.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn run_writes_a_report_docwizz_reads_and_leaks_no_credentials() {
    let base = serve().await;
    let (dir, test) = workdir("run");
    let out = dir.join("results/load");
    let o = cli(args(&[
        "run",
        &test,
        "--base-url",
        &base,
        "--accounts",
        "examples/mockapp/accounts.csv",
        "--out",
        out.to_str().unwrap(),
        "--details",
        "--format",
        "json",
        "--max-error-rate",
        "0.05",
        "--max-p95",
        "5000",
    ]))
    .await;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    assert_eq!(o.status.code(), Some(0), "{stderr}");
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout is the JSON report only");
    assert_eq!(report["schema"], "powerwizz.report/v1");
    assert!(report["totals"]["requests"].as_u64().unwrap() > 0);
    assert!(
        stderr.contains("ok   error rate") && stderr.contains("ok   p95"),
        "{stderr}"
    );
    let mut everything = format!("{stdout}{stderr}");
    for suffix in ["-bericht.json", "-zusammenfassung.csv", "-requests.csv"] {
        everything += &std::fs::read_to_string(format!("{}{suffix}", out.display())).expect(suffix);
    }
    // accounts.csv: user0,pw0 … user499,pw499.
    assert!(
        !everything.contains("user1") && !everything.contains("pw1"),
        "credentials leaked"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missed_threshold_exits_1_with_the_summary() {
    let base = serve().await;
    let (_, test) = workdir("threshold");
    let o = cli(args(&[
        "run",
        &test,
        "--base-url",
        &base,
        "--accounts",
        "examples/mockapp/accounts.csv",
        "--max-p95",
        "0",
    ]))
    .await;
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("FAIL p95"), "{stderr}");
    assert!(String::from_utf8_lossy(&o.stdout).contains("requests"));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_without_running() {
    let (_, test) = workdir("refuse");
    // Non-local target without --yes: refused before any request.
    let o = cli(args(&[
        "run",
        &test,
        "--base-url",
        "https://example.com/",
        "--accounts",
        "examples/mockapp/accounts.csv",
    ]))
    .await;
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
    // PerUser without accounts: the GUI's validation, exit 2.
    let o = cli(args(&["validate", &test])).await;
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("accounts imported"));
    let o = cli(args(&[
        "validate",
        &test,
        "--accounts",
        "examples/mockapp/accounts.csv",
    ]))
    .await;
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = cli(args(&["run", &test, "--bogus"])).await;
    assert_eq!(o.status.code(), Some(2));
}

#[tokio::test]
async fn version_is_one_line_for_gitwizz_doctor() {
    let o = cli(args(&["--version"])).await;
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("powerwizz {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn capacity_run_reports_every_stage_and_exits_0_at_max_users() {
    let base = serve().await;
    let (dir, test) = workdir("capacity");
    let out = dir.join("cap");
    let o = cli(args(&[
        "run",
        &test,
        "--base-url",
        &base,
        "--accounts",
        "examples/mockapp/accounts.csv",
        "--start-users",
        "2",
        "--max-users",
        "4",
        "--step-size",
        "2",
        "--stage-duration",
        "2",
        "--max-error-rate",
        "0.5",
        "--out",
        out.to_str().unwrap(),
        "--format",
        "json",
    ]))
    .await;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    assert_eq!(o.status.code(), Some(0), "{stderr}");
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout is the JSON report only");
    // The regular report is kept (it describes the last stage) and gains the capacity section.
    assert_eq!(report["schema"], "powerwizz.report/v1");
    assert_eq!(report["config"]["users"], 4);
    assert!(report["totals"]["requests"].as_u64().unwrap() > 0);
    let cap = &report["capacity"];
    assert_eq!(cap["stop_reason"], "max_users_reached");
    assert_eq!(cap["highest_passed_users"], 4);
    let users: Vec<_> = cap["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["users"].as_u64().unwrap(),
                s["result"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(users, [(2, "passed".into()), (4, "passed".into())]);
    assert!(stderr.contains("ok   capacity"), "{stderr}");
    let written = std::fs::read_to_string(format!("{}-bericht.json", out.display())).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&written).unwrap(),
        report,
        "the written report is the printed one"
    );
    assert!(
        !format!("{stdout}{stderr}{written}").contains("pw1"),
        "credentials leaked"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn capacity_threshold_violation_exits_1_and_stops() {
    let base = serve().await;
    let (_, test) = workdir("capacity-fail");
    let o = cli(args(&[
        "run",
        &test,
        "--base-url",
        &base,
        "--accounts",
        "examples/mockapp/accounts.csv",
        "--start-users",
        "2",
        "--max-users",
        "6",
        "--step-size",
        "2",
        "--stage-duration",
        "2",
        "--max-p95",
        "0",
    ]))
    .await;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    assert_eq!(o.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("FAIL capacity") && stderr.contains("threshold_exceeded"),
        "{stderr}"
    );
    assert_eq!(
        stderr.lines().filter(|l| l.starts_with("stage ")).count(),
        1,
        "escalated after a violation: {stderr}"
    );
    assert!(stdout.contains("stop: threshold_exceeded"), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn capacity_flags_are_validated_before_any_load() {
    let (_, test) = workdir("capacity-refuse");
    for bad in [
        // Not all four capacity flags.
        vec!["--start-users", "1", "--max-users", "4"],
        // No threshold.
        vec![
            "--start-users",
            "1",
            "--max-users",
            "4",
            "--step-size",
            "1",
            "--stage-duration",
            "1",
        ],
        // Beyond MAX_USERS.
        vec![
            "--start-users",
            "1",
            "--max-users",
            "501",
            "--step-size",
            "100",
            "--stage-duration",
            "1",
            "--max-p95",
            "1",
        ],
        // 60 stages of 2 min exceed the 1 h limit.
        vec![
            "--start-users",
            "1",
            "--max-users",
            "60",
            "--step-size",
            "1",
            "--stage-duration",
            "120",
            "--max-p95",
            "1",
        ],
    ] {
        let mut a = args(&["run", &test, "--accounts", "examples/mockapp/accounts.csv"]);
        a.extend(args(&bad));
        let o = cli(a).await;
        assert_eq!(
            o.status.code(),
            Some(2),
            "{bad:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fixed_load_report_has_no_capacity_section() {
    let base = serve().await;
    let (_, test) = workdir("fixed");
    let o = cli(args(&[
        "run",
        &test,
        "--base-url",
        &base,
        "--accounts",
        "examples/mockapp/accounts.csv",
        "--format",
        "json",
    ]))
    .await;
    assert_eq!(o.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["config"]["users"], 10);
    assert!(report.get("capacity").is_none());
}
