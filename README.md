# powerwizz

A small native Windows load tester for web applications, written in Rust with egui. Virtual users ramp up, log in
(shared account, or one account per user from a CSV, with session cookies or JWTs) and work through ordered HTTP
scenarios, while a live dashboard shows active users, RPS, error rate and p50/p95/p99 latency. Results export as CSV
or as a versioned JSON report that docwizz can read. `powerwizz-cli` runs the same test definitions headless, for CI
and gitwizz gates.

> **Only run load tests against systems you are authorized to test.** powerwizz asks for confirmation before every
> start and warns when the target isn't a local or private host; `powerwizz-cli` refuses such a target without `--yes`.

## Install

Download `powerwizz-vX.Y.Z-windows-x64.exe` (and `powerwizz-cli-vX.Y.Z-windows-x64.exe`) from the
[latest release](https://github.com/mycaravam-crypto/powerwizz/releases/latest). Each is a single exe with no runtime
dependencies, for Windows 10/11 x64.

To build it yourself:

```bash
cargo build --release                                     # on Windows (MSVC, Windows SDK)
rustup target add x86_64-pc-windows-gnu                   # from Linux/WSL, needs gcc-mingw-w64-x86-64
cargo build --release --target x86_64-pc-windows-gnu
```

## Quick start

[`examples/mockapp`](examples/mockapp/README.md) is an in-memory web shop to try it against (login, bearer tokens, a protected API, varying
latency, 1 % failing orders, a rate limit):

```bash
cargo run --example mockapp            # http://127.0.0.1:18080, users user0…user999 with password pw0…pw999
```

In powerwizz, under *testdefinition* load `examples/mockapp/test.json` (or `test-jwt.json` for bearer tokens), under
*anmeldung* pick *Account pro Benutzer* and import `examples/mockapp/accounts.csv`, then press **▶ start**.

A test definition sets the base URL, virtual users (max. 500), ramp-up, duration (max. 1 h) and scenarios: ordered HTTP
steps with headers, body, random pauses and expected status codes. Steps can use `{{username}}`, `{{password}}`,
`{{vu_id}}` and values extracted from earlier responses (`token = /access_token`, `header:Name`, `cookie:name`).
Definitions save without passwords, and no export ever contains credentials, tokens, cookies, headers or bodies.
The full walkthrough, token extraction and re-login rules: [docs/usage.md](docs/usage.md).

## Commands

The GUI is `powerwizz.exe`. `powerwizz-cli` is its console twin:

```text
powerwizz-cli run <test.json> [options]      run a test, print the result
powerwizz-cli validate <test.json> [--base-url u] [--accounts f] [--password-env VAR]
powerwizz-cli --version | help
```

| Option (`run`) | Meaning |
|---|---|
| `--base-url <url>` | override the definition's base URL |
| `--label <text>` | what the run measures (e.g. `staging, Release`); stored in the report |
| `--accounts <users.csv>` | `username,password` per virtual user |
| `--password-env <VAR>` | the shared account's password, from that environment variable |
| `--out <name>` | write `<name>-bericht.json` and `<name>-zusammenfassung.csv` |
| `--details` | also `<name>-requests.csv` (needs `--out`) |
| `--format text\|json` | stdout: a summary (default) or the `powerwizz.report/v1` report |
| `--max-error-rate <0..1>` | fail when the error rate is higher |
| `--max-p95 <ms>` | fail when the p95 latency is higher |
| `--yes` | you are authorized; required for a non-local target |

Progress goes to stderr, so stdout stays parseable. All options: [docs/usage.md](docs/usage.md#command-line-powerwizz-cli);
the report format: [docs/report.md](docs/report.md). As a gitwizz gate, with docwizz picking up the numbers:

```yaml
# .gitwizz.yml
gates:
  - id: load
    type: command
    run: powerwizz-cli run load/test.json --accounts load/users.csv --out load/run --max-error-rate 0.01 --max-p95 200
    blocking: false
```

```bash
docwizz load import . load/run-bericht.json
```

| Exit code | Meaning |
|---|---|
| `0` | done, thresholds met (`validate`: the test would start) |
| `1` | a threshold was missed |
| `2` | bad arguments, invalid definition or accounts, non-local target without `--yes`, unwritable output |

## Limitations

- Values can't be extracted from HTML, so form logins with a CSRF token fail with 403.
- Token expiry is noticed by a 401 (the user logs in again), not by reading the JWT's `exp`.
- Percentiles are accurate to about ±2.5 %; requests in flight at the end are not counted.
- Limits: 500 users, 1 h, 100 000 request details. Results stay in memory until exported.

All limitations: [docs/usage.md](docs/usage.md#known-limitations).

Measured on an i9-13900HK: 500 users against the mock shop used 45 MB RAM with no measurable latency overhead; without
pauses, about 30 000–45 000 requests per second. Conditions and full tables: [docs/measurements.md](docs/measurements.md).

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                                  # engine, end-to-end against the mock shop, and the CLI; no external server
```

Every merge to `main` is released as the next SemVer tag with both exes and `SHA256SUMS.txt`; a PR label picks the
bump: `release:patch` (default), `release:minor`, `release:major` or `release:none`. Build details, test suites and CI: [docs/development.md](docs/development.md). Design notes:
[PLAN.md](PLAN.md).
