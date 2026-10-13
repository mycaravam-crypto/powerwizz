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

## Tests

`cargo test` runs unit tests and integration tests. Integration tests use a tiny in-process HTTP/1.1 server (`tests/common/mod.rs`) on a random local port, so no external server is needed.
