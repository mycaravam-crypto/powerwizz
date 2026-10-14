# powerwizz

A small native Windows load tester for web applications. It is written in Rust with egui, in the dense dark style of
reswizz. **Virtual users** run as async tasks, each with its own cookie jar and login. They ramp up over a set time and
work through ordered HTTP scenarios with random pauses. A **live dashboard** shows active users, requests per second,
error rate, p50/p95/p99 latency and the latest errors. Logins can use a shared account or **one account per user** from
a CSV, with session cookies or **JWTs** taken from the login response. Results export as CSV or as a versioned
**JSON report** that docwizz can read. **`powerwizz-cli`** runs the same test definitions headless, for CI and gitwizz
gates.

> **Only run load tests against systems you are authorized to test.** powerwizz asks for confirmation before every
> start and warns when the target isn't a local or private host. `powerwizz-cli` refuses such a target unless you pass
> `--yes`.

## Quick start

Download `powerwizz-vX.Y.Z-windows-x64.exe` from the [latest release](https://github.com/mycaravam-crypto/powerwizz/releases/latest)
and start it. To try it without a target of your own, run the bundled mock web shop (see [below](#try-it-mock-web-shop)):

    cargo run --example mockapp

In powerwizz, under *testdefinition*, load `examples/mockapp/test.json`. Under *anmeldung*, pick
*Account pro Benutzer* and import `examples/mockapp/accounts.csv`. Then press **▶ start** and confirm.

## Using it

1. **Load.** Set the base URL, virtual users (max. 500), ramp-up, duration (max. 1 h), an optional request limit and
   the request timeout. The test ends at the duration, the request limit or **■ stop**, whichever comes first.
2. **Scenarios.** A scenario is an ordered list of HTTP steps: method, path or URL, headers as `Name: value` lines,
   body, a random pause range, and expected status codes (empty means 2xx). Each iteration picks a scenario at random.
   A failed step ends the iteration and counts the error; requests are never retried.
3. **Login.** No login, one shared account, or one account per user from a CSV (`username,password`). Login and
   registration steps run once per user and may use `{{username}}`, `{{password}}` and `{{vu_id}}`.
4. **Start.** A confirmation shows target, load, scenarios and login. While the test runs the configuration is
   read-only.
5. **Results.** After the test, export `<name>-zusammenfassung.csv` (total and per step), `<name>-requests.csv` (each
   request, if *details* was on, max. 100 000 rows) or `<name>-bericht.json`.

Test definitions save as JSON **without passwords**. Credentials stay in memory only.

### Tokens and JWT

**Extract.** A step's *extrahieren* field takes values from its response, one `name = source` per line. Later
requests of the same user use them as `{{name}}`, usually as the default header `Authorization: Bearer {{token}}`.

| Source | Example | Takes |
|---|---|---|
| JSON pointer | `token = /access_token` | a value from the JSON body |
| `header:Name` | `token = header:Authorization` | the first header of that name (case-insensitive), without a leading `Bearer ` |
| `cookie:name` | `token = cookie:jwt` | the value of that `Set-Cookie`, without attributes |

**Not yet filled** placeholders leave their header out, so the default `Authorization` header doesn't break the
login request itself. **Unknown placeholders** (a typo like `{{tokn}}`) block the start.

**Expiry.** When a scenario step gets an unexpected 401 and the test has login steps, the user logs in again and goes
on with the next iteration. The 401 still counts as an error. A second 401 right after a new login is handled like any
other error, so a broken login can't turn into a login storm.

**Throttling.** A login step answered 429 (a login rate limit, which many users from one machine hit quickly) is
tried again after the server's `Retry-After`, or after 1, 2, 4, 8 and 16 s, at most five times. Each 429 still counts
as an error. A user whose login fails puts no load on the target, so the report counts `users_logged_in` next to
`users_started`. Against a per-IP limit of n logins a minute, ramp up over at least users / n minutes.

### What never leaves the app

Records, CSV exports, the JSON report and error messages never contain passwords, usernames, extracted tokens,
cookies, headers, bodies or query strings. Paths come from the step template (`/users/{{vu_id}}`), not the filled-in
URL.

## Try it: mock web shop

`examples/mockapp` is a small in-memory web shop to test against. It has a JSON login with session cookie, expiring
bearer tokens, a protected API with route parameters, varying latency, 1 % failing orders, a per-session rate limit
and a CSRF-protected form.

    cargo run --example mockapp                    # http://127.0.0.1:18080
    cargo run --example mockapp -- 0.0.0.0:9000    # another address

It accepts `user0`…`user999` with password `pw0`…`pw999`. `accounts.csv` has the first 500. Two ready test
definitions: `test.json` (session cookie) and `test-jwt.json` (bearer token from `/api/token`).

| Route | Behaviour |
|---|---|
| `POST /api/login` | JSON `{"username","password"}` → 200 and `sid` cookie, 401 on bad credentials |
| `POST /api/token` | same credentials → `{"access_token","token_type":"Bearer","expires_in":300}`, a JWT-shaped token that expires |
| `POST /api/token?via=header` | same, but 204 with the token only in `Authorization: Bearer …` and `Set-Cookie: jwt=…` |
| `GET /api/items`, `GET /api/items/{id}` | 5–40 ms latency, 404 for id ≥ 1000 |
| `POST /api/orders` | JSON `{"item","qty"}` → 201, 400 on bad input, 500 for 1 % |
| `GET /api/slow` | 300–800 ms |
| `POST /api/logout` | 204 |
| `GET /login`, `POST /login` | HTML form with a CSRF token; without it → 403 (see [limitations](#known-limitations)) |

Every `/api/*` route except login and token answers 401 without a session cookie or valid bearer token, and 429
above 20 requests per second and session.

## Command line: powerwizz-cli

`powerwizz-cli` is the headless twin of the app, for CI, [gitwizz](https://github.com/mycaravam-crypto/gitwizz) gates
and [docwizz](https://github.com/mycaravam-crypto/docwizz). It reads the test definitions the app saves, validates them
with the same rules and runs the same engine. It is a separate console exe because the app is a Windows GUI program,
which has no console to print to.

    powerwizz-cli run <test.json> [options]      run a test, print the result
    powerwizz-cli validate <test.json> [--base-url u] [--accounts f] [--password-env VAR]
    powerwizz-cli --version                      powerwizz X.Y.Z
    powerwizz-cli help

| Option (`run`) | Meaning |
|---|---|
| `--base-url <url>` | override the definition's base URL, e.g. per environment |
| `--label <text>` | what the run measures, e.g. `staging, Release, 10k orders`; into the report as `label` (overrides the definition's *umgebung*) |
| `--accounts <users.csv>` | `username,password` per virtual user (*Account pro Benutzer*) |
| `--password-env <VAR>` | the shared account's password, from that environment variable; never an argument, never in the file |
| `--out <name>` | write `<name>-bericht.json` and `<name>-zusammenfassung.csv`; a missing folder is created before the run |
| `--details` | also `<name>-requests.csv` (needs `--out`) |
| `--format text\|json` | stdout: a summary (default) or the `powerwizz.report/v1` report alone |
| `--max-error-rate <0..1>` | fail when the total error rate is higher |
| `--max-p95 <ms>` | fail when the total p95 latency is higher; with a threshold, a run without any request fails too, and so does one where fewer users logged in than started (without a threshold that is a warning) |
| `--yes` | you are authorized; required when the target isn't a local or private host |

Progress (every 5 s), threshold verdicts and written files go to stderr, so stdout stays parseable. Ctrl+C stops the
run like **■ stop** and still prints and writes the results.

| Exit code | Meaning |
|---|---|
| `0` | done, thresholds met (`validate`: the test would start) |
| `1` | a threshold was missed |
| `2` | bad arguments, an invalid definition or accounts, a non-local target without `--yes`, an unwritable output |

**In the ecosystem.** A gitwizz gate runs the test and fails the PR on the thresholds; `gitwizz doctor` checks that
`powerwizz-cli --version` works. docwizz then puts the measured numbers next to each endpoint in `api.md` and its HTML:

```yaml
# .gitwizz.yml
gates:
  - id: load
    type: command
    run: powerwizz-cli run load/test.json --accounts load/users.csv --out load/run --max-error-rate 0.01 --max-p95 200
    blocking: false
```

    docwizz load import . load/run-bericht.json
    docwizz generate . --html

## JSON report for docwizz

`<name>-bericht.json` is a versioned, machine-readable result (`powerwizz.report/v1`), in the same spirit as
`gitwizz.history/v2`. docwizz can attach it to its endpoint docs
([docwizz#185](https://github.com/mycaravam-crypto/docwizz/issues/185)). The target URL is stripped of userinfo and
query. Latencies are in milliseconds; keys come out in alphabetical order.

From a 60 s run of `test.json` with 50 users against the mock shop, trimmed to one step and one series point:

```json
{
  "schema": "powerwizz.report/v1",
  "generator": "powerwizz 0.1.0",
  "started_at": "2026-10-13T18:39:16Z",
  "runtime_s": 60.0,
  "target": { "base_url": "http://127.0.0.1:18080/", "local": true },
  "label": "mock shop, local",
  "config": { "users": 50, "ramp_up_s": 10, "duration_s": 60, "request_limit": null,
              "timeout_s": 10, "auth": "PerUser", "scenarios": ["browse", "order"] },
  "totals": { "users_started": 50, "users_logged_in": 50, "iterations": 1808, "requests": 3758, "failures": 8,
              "error_rate": 0.0021287919105907396, "rps": 62.629926452180285,
              "latency_ms": { "mean": 29.778, "p50": 26.679, "p95": 67.417, "p99": 81.946 },
              "status_counts": { "200": 2814, "201": 936, "500": 8 }, "error_kinds": { "status": 8 } },
  "steps": [ { "name": "browse/detail", "method": "GET", "path": "/api/items/{{vu_id}}",
               "requests": 898, "failures": 0, "error_rate": 0.0,
               "latency_ms": { "mean": 24.295, "p50": 24.198, "p95": 39.417, "p99": 41.388 } } ],
  "series": [ { "t": 1, "requests": 9, "failures": 0, "p50_ms": 2.009, "p95_ms": 43.457, "p99_ms": 43.457 } ]
}
```

- **Label:** the definition's *umgebung* or `--label`, trimmed, at most 200 characters; `null` when empty. Say where
  the numbers come from (a dev stack, a Debug build, a small database), so they aren't read as production figures.
- **Endpoint:** `steps[].method` and `steps[].path`. The path is the step's template, resolved against the base URL,
  so placeholders stay literal.
- **Series:** one point per second (`t` is the end of the second), at most 3700.
- **Versioning:** v1 may gain fields. Renamed or removed fields bump the schema version.

## Build

The app targets Windows 10/11 x64. Two ways to build it:

**On Windows** (MSVC toolchain, with the Windows SDK installed through Visual Studio Build Tools):

    cargo build --release

**From Linux/WSL** (needs mingw-w64: `apt install gcc-mingw-w64-x86-64`):

    rustup target add x86_64-pc-windows-gnu
    cargo build --release --target x86_64-pc-windows-gnu

The output is a single `powerwizz.exe` with no runtime dependencies. It only talks to the targets you configure.

## Checks

    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

The integration tests need no external server:

- **Engine edge cases** (`tests/engine.rs`, `tests/http_step.rs`): a tiny in-process HTTP server with synthetic routes
  for status codes, delays, cookies and dropped connections. They cover ramp-up timing, the user limit, stop, timeouts,
  the request limit and cookie isolation between users.
- **End to end** (`tests/mockapp.rs`): the shipped samples against the mock shop. Per-user login, all scenarios clean,
  401 without login, 429 and 500 counted, CSRF form rejected, JWT from JSON, header and cookie, token expiry with
  re-login, and no token in any export.
- **CLI** (`tests/cli.rs`): the real `powerwizz-cli` binary against the mock shop: report and exports written, stdout
  only JSON with `--format json`, thresholds, refusals and no credentials anywhere in its output.

## Measurements

Measured 2026-10-13 on Windows 11 Pro, Intel i9-13900HK (20 logical cores), 32 GB RAM. Test binaries were cross-built
with `x86_64-pc-windows-gnu`; peak working set and CPU time were read with PowerShell. Engine and target share one
process, so CPU and RAM include both.

**Realistic: the mock shop.** The shipped `test.json` with its real pauses (0.3–1.5 s), per-user login, default mock
behaviour, 10 s ramp-up, 60 s:

    BENCH_USERS=500 BENCH_SECS=60 cargo test --release --test mockapp -- --ignored --nocapture

| VUs | Requests | Ø RPS | p50 | p95 | p99 | Failures (500) | Peak RAM | CPU time |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 3 692 | 61.5 | 32.4 ms | 78.0 ms | 81.9 ms | 11 | 13.4 MB | 3.2 s |
| 200 | 14 779 | 246.3 | 32.4 ms | 78.0 ms | 81.9 ms | 28 | 23.9 MB | 9.1 s |
| 500 | 36 941 | 615.4 | 32.4 ms | 78.0 ms | 81.9 ms | 91 | 45.4 MB | 20.9 s |

Latency equals the mock's configured delays at every load level, so the generator adds no measurable overhead.

**Throughput: no pauses.** One `GET /` step against the minimal test server, 2 s ramp-up, 15 s:

    BENCH_USERS=200 BENCH_SECS=15 cargo test --release --test bench -- --ignored --nocapture

| VUs | Requests | Ø RPS | p50 | p95 | p99 | Peak RAM | CPU time |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 687 831 | 45 852 | 0.92 ms | 2.01 ms | 2.83 ms | 69.7 MB | 81.3 s |
| 200 | 599 772 | 39 961 | 4.18 ms | 9.12 ms | 11.64 ms | 74.9 MB | 59.6 s |
| 500 | 471 776 | 31 414 | 14.86 ms | 28.01 ms | 34.05 ms | 71.5 MB | 110.3 s |

RAM stays flat as users and requests grow, because all metrics are bounded. The GUI idles at about 122 MB with the
glow (OpenGL) renderer, against 393 MB with egui's default wgpu renderer.

## CI and releases

GitHub Actions run on every pull request and on `main`:

- **test**: `fmt`, `clippy -D warnings` and `cargo test` on Linux; `clippy`, `cargo test` and the release build on Windows.
- **security**: Trivy scans `Cargo.lock` for CVEs (weekly too). A fixable CRITICAL fails the job.
- **release**: each merged PR into `main` tags the next SemVer version and publishes a GitHub release. The release has
  `powerwizz-vX.Y.Z-windows-x64.exe`, `powerwizz-cli-vX.Y.Z-windows-x64.exe` and `SHA256SUMS.txt`. Git tags are the
  source of truth. A PR label picks the bump: `release:patch` (the default), `release:minor`, `release:major`, or `release:none` to skip the release.
  The version shows in the window title, in `powerwizz-cli --version` and in the JSON report.

## Known limitations

- **No values from HTML.** Extraction reads JSON bodies, response headers and `Set-Cookie`. A CSRF token inside an
  HTML form can't be carried into the next request, so form logins with CSRF protection fail with 403.
- **Placeholders are plain names.** `{{username}}`, `{{password}}`, `{{vu_id}}` and extracted names; no expressions,
  regex or partial header values.
- **Token expiry is noticed by a 401,** not by reading the JWT's `exp`. The request that got the 401 counts as failed.
- **Percentiles are accurate to about ±2.5 %.** They come from a fixed log histogram, clamped to 100 µs–60 s.
- **Requests still in flight at the end or on stop** are aborted and not counted.
- **Fixed limits:** 500 users, 1 h, 100 000 request details, the last 200 errors.
- **Results live in memory only** until you export them. Closing the app discards them.
- **File paths are typed into text fields;** there is no file dialog.

Design notes and the original plan are in [PLAN.md](PLAN.md).
