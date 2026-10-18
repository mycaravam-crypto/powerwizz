# JSON report for docwizz

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
