# Measurements

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
