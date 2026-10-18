# Using powerwizz

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

## Tokens and JWT

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

## What never leaves the app

Records, CSV exports, the JSON report and error messages never contain passwords, usernames, extracted tokens,
cookies, headers, bodies or query strings. Paths come from the step template (`/users/{{vu_id}}`), not the filled-in
URL.

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
