//! `powerwizz-cli`: headless runs for CI, gitwizz gates and `docwizz load import` (#36). Same validation, engine and
//! exports as the GUI. Progress and messages go to stderr; stdout carries only the result (text, or the JSON report).

use std::path::Path;
use std::time::Duration;

use reqwest::Url;

use crate::engine::{self, Shared};
use crate::model::{self, Account, AuthStrategy, TestDefinition};

pub const VERSION: &str = concat!("powerwizz ", env!("CARGO_PKG_VERSION"));

pub const USAGE: &str = "\
powerwizz-cli: headless load tests for CI. Only load-test systems you are authorized to test.

usage:
  powerwizz-cli run <test.json> [options]   run a test definition, print the result
  powerwizz-cli validate <test.json> [--base-url u] [--accounts f] [--password-env VAR]
                                            check it would start, without sending a request
  powerwizz-cli --version | version
  powerwizz-cli help

run options:
  --base-url <url>          override the definition's base URL (e.g. per environment)
  --accounts <users.csv>    username,password per virtual user (auth PerUser)
  --password-env <VAR>      the shared account's password, from this environment variable
  --out <name>              write <name>-bericht.json (powerwizz.report/v1) and <name>-zusammenfassung.csv
  --details                 also write <name>-requests.csv (max. 100 000 rows)
  --format text|json        stdout: a summary (default) or the powerwizz.report/v1 report
  --max-error-rate <0..1>   exit 1 when the total error rate is higher
  --max-p95 <ms>            exit 1 when the total p95 latency is higher
  --yes                     confirm you are authorized; required for a target that isn't local or private

exit codes: 0 done (thresholds met) · 1 a threshold missed · 2 usage, invalid input or refused target";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    /// "run", "validate", "version" or "help".
    pub command: String,
    pub file: String,
    pub base_url: Option<String>,
    pub accounts: Option<String>,
    pub password_env: Option<String>,
    pub out: Option<String>,
    pub details: bool,
    pub json: bool,
    pub max_error_rate: Option<f64>,
    pub max_p95_ms: Option<f64>,
    pub yes: bool,
}

/// Parses the arguments after the program name. `--flag value` and `--flag=value` both work.
pub fn parse(args: &[String]) -> Result<Args, String> {
    if args.iter().any(|a| a == "--version" || a == "-V")
        || args.first().is_some_and(|a| a == "version")
    {
        return Ok(Args {
            command: "version".into(),
            ..Default::default()
        });
    }
    if args.is_empty()
        || args
            .iter()
            .any(|a| matches!(a.as_str(), "help" | "-h" | "--help"))
    {
        return Ok(Args {
            command: "help".into(),
            ..Default::default()
        });
    }
    let mut a = Args {
        command: args[0].clone(),
        ..Default::default()
    };
    if !matches!(a.command.as_str(), "run" | "validate") {
        return Err(format!(
            "unknown command '{}' (try: run, validate, version, help)",
            a.command
        ));
    }
    let mut files = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let Some(flag) = arg.strip_prefix("--") else {
            files.push(arg.clone());
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (flag, None),
        };
        if a.command == "validate" && !matches!(name, "base-url" | "accounts" | "password-env") {
            return Err(format!("validate doesn't take --{name}"));
        }
        let mut value = || {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or(format!("--{name} needs a value"))
        };
        let number = |v: String| v.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0);
        match name {
            "base-url" => a.base_url = Some(value()?),
            "accounts" => a.accounts = Some(value()?),
            "password-env" => a.password_env = Some(value()?),
            "out" => a.out = Some(value()?),
            "details" => a.details = true,
            "yes" => a.yes = true,
            "format" => {
                a.json = match value()?.as_str() {
                    "json" => true,
                    "text" => false,
                    f => return Err(format!("--format must be text or json, not '{f}'")),
                }
            }
            "max-error-rate" => {
                let v = value()?;
                a.max_error_rate = Some(
                    number(v.clone())
                        .filter(|n| *n <= 1.0)
                        .ok_or(format!("--max-error-rate must be a ratio 0..1, not '{v}'"))?,
                );
            }
            "max-p95" => {
                let v = value()?;
                a.max_p95_ms = Some(
                    number(v.clone())
                        .ok_or(format!("--max-p95 must be milliseconds, not '{v}'"))?,
                );
            }
            _ => return Err(format!("unknown option --{name}")),
        }
    }
    match files.as_slice() {
        [f] => a.file = f.clone(),
        [] => return Err(format!("{} needs a test definition (test.json)", a.command)),
        _ => {
            return Err(format!(
                "{} takes one test definition, got {}",
                a.command,
                files.len()
            ));
        }
    }
    if a.details && a.out.is_none() {
        return Err("--details needs --out".into());
    }
    Ok(a)
}

/// Runs the parsed command and returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("powerwizz-cli: {e}\n\n{USAGE}");
            return 2;
        }
    };
    match a.command.as_str() {
        "version" => println!("{VERSION}"),
        "help" => println!("{USAGE}"),
        _ => {
            let (def, accounts) = match prepare(&a) {
                Ok(x) => x,
                Err(errs) => {
                    for e in errs {
                        eprintln!("powerwizz-cli: {e}");
                    }
                    return 2;
                }
            };
            if a.command == "validate" {
                println!(
                    "{}: valid · {} users · {} s · {} scenario(s) · auth {:?}",
                    a.file,
                    def.users,
                    def.duration_secs,
                    def.scenarios.len(),
                    def.auth
                );
                return 0;
            }
            return run(&a, def, accounts);
        }
    }
    0
}

/// The definition with the CLI's overrides and secrets applied, and the accounts; or why it can't start.
fn prepare(a: &Args) -> Result<(TestDefinition, Vec<Account>), Vec<String>> {
    let mut def = TestDefinition::load(&a.file).map_err(|e| vec![e])?;
    if let Some(url) = &a.base_url {
        def.base_url = url.clone();
    }
    if let Some(var) = &a.password_env {
        def.shared_password =
            std::env::var(var).map_err(|_| vec![format!("--password-env: ${var} is not set")])?;
    } else if def.auth == AuthStrategy::Shared {
        // Definitions are saved without passwords, so a shared login always needs one from outside.
        return Err(vec![
            "auth Shared needs the password: --password-env <VAR>".into(),
        ]);
    }
    let accounts = match &a.accounts {
        Some(path) => model::load_accounts(path).map_err(|e| vec![format!("{path}: {e}")])?,
        None => Vec::new(),
    };
    match engine::check(&def, &accounts) {
        errs if errs.is_empty() => Ok((def, accounts)),
        errs => Err(errs),
    }
}

/// `host[:port]` only: the base URL may carry userinfo or a query.
fn host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            Some(format!(
                "{}{}",
                u.host_str()?,
                u.port().map(|p| format!(":{p}")).unwrap_or_default()
            ))
        })
        .unwrap_or_default()
}

fn run(a: &Args, def: TestDefinition, accounts: Vec<Account>) -> i32 {
    let target = host(&def.base_url);
    if !model::is_local_host(&def.base_url) && !a.yes {
        eprintln!(
            "powerwizz-cli: {target} is not a local or private host; pass --yes if you are authorized to load-test it"
        );
        return 2;
    }
    if let Some(dir) = a
        .out
        .as_deref()
        .and_then(|o| Path::new(o).parent())
        .filter(|d| !d.as_os_str().is_empty())
    {
        // Fail now, not after an hour of load.
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("powerwizz-cli: {}: {e}", dir.display());
            return 2;
        }
    }
    eprintln!(
        "{VERSION} · {target} · {} users · ramp-up {} s · {} s · only load-test systems you are authorized to test",
        def.users, def.ramp_up_secs, def.duration_secs
    );
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let shared = rt.block_on(async {
        let mut h = engine::start(def, accounts, a.details).expect("checked in prepare");
        let shared = h.shared.clone();
        let ticker = shared.clone();
        let progress = tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(5));
            every.tick().await;
            loop {
                every.tick().await;
                eprintln!("{}", progress_line(&ticker));
            }
        });
        tokio::select! {
            _ = h.wait() => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!("powerwizz-cli: stopping, results so far are kept");
                h.stop();
                h.wait().await;
            }
        }
        progress.abort();
        shared
    });

    let mut code = 0;
    let runtime = shared.runtime();
    {
        let m = shared.metrics.lock().unwrap();
        let p95 = m.latency.percentile(0.95).as_secs_f64() * 1e3;
        let mut gate = |name: &str, ok: bool, detail: String| {
            eprintln!("{} {name}: {detail}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                code = 1;
            }
        };
        if (a.max_error_rate.is_some() || a.max_p95_ms.is_some()) && m.requests == 0 {
            gate("thresholds", false, "no request completed".into());
        }
        if let Some(max) = a.max_error_rate {
            let rate = m.error_rate();
            gate(
                "error rate",
                rate <= max,
                format!("{:.2} % (max {:.2} %)", rate * 100.0, max * 100.0),
            );
        }
        if let Some(max) = a.max_p95_ms {
            gate("p95", p95 <= max, format!("{p95:.1} ms (max {max:.1} ms)"));
        }
        if !a.json {
            println!("{}", summary(&shared, &m, runtime));
        }
    }
    if a.json {
        println!("{}", shared.report_json());
    }
    if let Some(out) = &a.out {
        let report = shared.report_json();
        let m = shared.metrics.lock().unwrap();
        let mut files = vec![
            (format!("{out}-bericht.json"), report),
            (
                format!("{out}-zusammenfassung.csv"),
                crate::metrics::summary_csv(&m, runtime),
            ),
        ];
        if a.details {
            files.push((
                format!("{out}-requests.csv"),
                crate::metrics::records_csv(&m),
            ));
        }
        for (path, text) in files {
            match std::fs::write(&path, text) {
                Ok(()) => eprintln!("wrote {path}"),
                Err(e) => {
                    eprintln!("powerwizz-cli: {path}: {e}");
                    code = 2;
                }
            }
        }
    }
    code
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn progress_line(s: &Shared) -> String {
    let m = s.metrics.lock().unwrap();
    format!(
        "{:>5.0} s  users {}/{}  requests {}  rps {}  errors {:.2} %  p95 {:.1} ms",
        s.elapsed().as_secs_f64(),
        s.active.load(std::sync::atomic::Ordering::Relaxed),
        s.users,
        m.requests,
        m.current_rps(),
        m.error_rate() * 100.0,
        ms(m.latency.percentile(0.95))
    )
}

fn summary(s: &Shared, m: &crate::metrics::Metrics, runtime: Duration) -> String {
    let secs = runtime.as_secs_f64();
    let mut out = format!(
        "{:.1} s · {} users · {} requests · {:.1} rps · {} failures ({:.2} %)\n\
         total  p50 {:.1} ms  p95 {:.1} ms  p99 {:.1} ms\n",
        secs,
        s.started.load(std::sync::atomic::Ordering::Relaxed),
        m.requests,
        m.requests as f64 / secs.max(0.001),
        m.failures,
        m.error_rate() * 100.0,
        ms(m.latency.percentile(0.50)),
        ms(m.latency.percentile(0.95)),
        ms(m.latency.percentile(0.99)),
    );
    for (name, st) in &m.per_step {
        let rate = if st.requests == 0 {
            0.0
        } else {
            st.failures as f64 / st.requests as f64
        };
        out += &format!(
            "{name}  {} {}  {} requests  {:.2} %  p50 {:.1}  p95 {:.1}  p99 {:.1} ms\n",
            st.method,
            st.path,
            st.requests,
            rate * 100.0,
            ms(st.latency.percentile(0.50)),
            ms(st.latency.percentile(0.95)),
            ms(st.latency.percentile(0.99)),
        );
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Args, String> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn parses_run_options() {
        let a = p("run t.json --base-url=http://x --accounts a.csv --out o --details --format json --max-error-rate 0.01 --max-p95 200 --yes").unwrap();
        assert_eq!(a.file, "t.json");
        assert_eq!(a.base_url.as_deref(), Some("http://x"));
        assert_eq!((a.details, a.json, a.yes), (true, true, true));
        assert_eq!((a.max_error_rate, a.max_p95_ms), (Some(0.01), Some(200.0)));
    }

    #[test]
    fn version_and_help_win() {
        assert_eq!(p("run t.json --version").unwrap().command, "version");
        assert_eq!(p("").unwrap().command, "help");
        assert_eq!(p("run --help").unwrap().command, "help");
    }

    #[test]
    fn rejects_bad_usage() {
        for bad in [
            "go t.json",
            "run",
            "run a.json b.json",
            "run t.json --nope",
            "run t.json --out",
            "run t.json --format xml",
            "run t.json --max-error-rate 2",
            "run t.json --max-p95 -1",
            "run t.json --details",
            "validate t.json --out o",
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn host_drops_userinfo_and_query() {
        assert_eq!(
            host("https://u:pw@shop.example:8443/app?key=s"),
            "shop.example:8443"
        );
    }
}
