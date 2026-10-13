# powerwizz

Small native Windows tool for load testing web applications with virtual users. See [PLAN.md](PLAN.md) and the [v1 milestone](https://github.com/mycaravam-crypto/powerwizz/milestone/1).

> **Only run load tests against systems you are authorized to test.**

## Build and run

Requires Rust stable (edition 2024).

```sh
cargo run              # debug build, opens the GUI
cargo build --release  # target/release/powerwizz.exe on Windows
```

CI (GitHub Actions, `windows-latest`) runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and a release build. The release EXE is uploaded as the `powerwizz-windows-x64` artifact.

## Usage

1. Set base URL, virtual users, ramp-up, duration, optional request limit and timeout.
2. Edit scenarios: ordered HTTP steps (method, path or URL, headers as `Name: value` lines, body, pause range, expected status codes; empty = 2xx). Each iteration picks a scenario at random.
3. Optional login: no login, shared account, or one account per virtual user imported from a CSV (`username,password`). Login/registration steps run once per user and may use `{{username}}`, `{{password}}` and `{{vu_id}}`.
4. Start shows a confirmation with target and load; non-local hosts get an extra warning.

5. After the test, export CSV: `<name>-zusammenfassung.csv` (total + per step: requests, failures, error rate, RPS, mean/p50/p95/p99, status codes and error kinds) and, if request details were enabled, `<name>-requests.csv` (max. 100 000 rows).

Test definitions are saved as JSON without passwords. Credentials stay in memory only. Exports never contain passwords, cookies, headers, bodies, error messages or query strings; paths come from the step template (`/user/{{username}}`), not the filled-in URL.

Percentiles come from a fixed log histogram (100 µs–60 s); they are accurate to about ±2.5 %.

## Tests

`cargo test` runs unit tests and integration tests. Integration tests use a tiny in-process HTTP/1.1 server (`tests/common/mod.rs`) on a random local port, so no external server is needed.
