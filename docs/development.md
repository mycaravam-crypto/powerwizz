# Development

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

## CI and releases

GitHub Actions run on every pull request and on `main`:

- **test**: `fmt`, `clippy -D warnings` and `cargo test` on Linux; `clippy`, `cargo test` and the release build on Windows.
- **security**: Trivy scans `Cargo.lock` for CVEs (weekly too). A fixable CRITICAL fails the job.
- **release**: each merged PR into `main` tags the next SemVer version and publishes a GitHub release. The release has
  `powerwizz-vX.Y.Z-windows-x64.exe`, `powerwizz-cli-vX.Y.Z-windows-x64.exe` and `SHA256SUMS.txt`. Git tags are the
  source of truth. A PR label picks the bump: `release:patch` (the default), `release:minor`, `release:major`, or `release:none` to skip the release.
  The version shows in the window title, in `powerwizz-cli --version` and in the JSON report.
