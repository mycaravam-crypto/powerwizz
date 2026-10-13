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

## Measurements

Benchmark: `tests/bench.rs` (ignored by default) runs the engine against the in-process test server; engine and server share one process, so CPU and RAM include both. One step `GET /`, no pauses (closed loop, maximum throughput), 2 s ramp-up, 15 s duration, request details off.

```sh
BENCH_USERS=200 BENCH_SECS=15 cargo test --release --test bench -- --ignored --nocapture
```

Measured 2026-10-13 on Windows 11 Pro, Intel i9-13900HK (20 logical cores), 32 GB RAM; test binary cross-built with `x86_64-pc-windows-gnu`, peak working set and CPU time read via PowerShell.

| VUs | Requests | Ø RPS | p50 | p95 | p99 | Failures | Peak RAM | CPU time |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 687 831 | 45 852 | 0.92 ms | 2.01 ms | 2.83 ms | 0 | 69.7 MB | 81.3 s |
| 200 | 599 772 | 39 961 | 4.18 ms | 9.12 ms | 11.64 ms | 0 | 74.9 MB | 59.6 s |
| 500 | 471 776 | 31 414 | 14.86 ms | 28.01 ms | 34.05 ms | 0 | 71.5 MB | 110.3 s |

RAM stays flat with users and request count (bounded metrics). Latency growth reflects the shared CPU between generator and test server, not the network. The GUI release build idles at ~122 MB working set with the glow (OpenGL) renderer (~393 MB with the default wgpu renderer, hence glow).

## Known limitations

- No value extraction from responses: logins that need a CSRF token from a previous response are not supported.
- Only `{{username}}`, `{{password}}`, `{{vu_id}}` placeholders.
- Percentiles ±2.5 % (log histogram, clamped to 100 µs–60 s).
- Requests in flight when the test ends or is stopped are aborted and not counted.
- `MAX_USERS = 500`, max duration 1 h, request details capped at 100 000 rows, last 200 errors kept.
- Metrics are shared via one mutex between aggregator and GUI; fine at the measured rates.
- File paths are typed into text fields (no file dialog).
