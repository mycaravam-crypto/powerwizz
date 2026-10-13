use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use crate::engine::{ErrorKind, RequestRecord};

// Log-scale buckets from 100 µs to 60 s, each 5 % wider than the previous.
// Values are reported as the bucket's geometric midpoint, so a percentile is
// within ~2.5 % of the true sample value (clamped at both ends of the range).
const MIN_US: f64 = 100.0;
const MAX_US: f64 = 60_000_000.0;
const GROWTH: f64 = 1.05;
const BUCKETS: usize = 275; // ceil(ln(MAX/MIN) / ln(GROWTH)) + 1

pub const MAX_ERRORS: usize = 200;
pub const MAX_RECORDS: usize = 100_000;
/// One point per second; max test duration plus slack for draining.
const MAX_SERIES: usize = 3700;

/// Fixed-size latency histogram: constant memory regardless of request count.
#[derive(Clone, Debug)]
pub struct Histogram {
    counts: Box<[u64; BUCKETS]>,
    count: u64,
    sum_us: u128,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram {
            counts: Box::new([0; BUCKETS]),
            count: 0,
            sum_us: 0,
        }
    }
}

impl Histogram {
    pub fn add(&mut self, d: Duration) {
        let us = d.as_micros();
        let idx = if (us as f64) <= MIN_US {
            0
        } else {
            (((us as f64 / MIN_US).ln() / GROWTH.ln()) as usize).min(BUCKETS - 1)
        };
        self.counts[idx] += 1;
        self.count += 1;
        self.sum_us += us;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn mean(&self) -> Duration {
        match self.count {
            0 => Duration::ZERO,
            n => Duration::from_micros((self.sum_us / n as u128) as u64),
        }
    }

    /// `p` in 0..=1. Nearest-rank percentile; Duration::ZERO when empty.
    pub fn percentile(&self, p: f64) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let rank = ((p * self.count as f64).ceil() as u64).clamp(1, self.count);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                let mid = MIN_US * GROWTH.powf(i as f64 + 0.5);
                return Duration::from_micros(mid.min(MAX_US) as u64);
            }
        }
        unreachable!()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Point {
    /// Seconds since test start (window end).
    pub t: u64,
    pub requests: u64,
    pub failures: u64,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct StepStats {
    /// Method and path template of the step, e.g. "GET" and "/api/items/{{vu_id}}".
    pub method: &'static str,
    pub path: String,
    pub requests: u64,
    pub failures: u64,
    pub latency: Histogram,
}

/// All results of one test run. Every collection is bounded.
#[derive(Debug, Default)]
pub struct Metrics {
    pub requests: u64,
    pub failures: u64,
    pub latency: Histogram,
    pub by_kind: BTreeMap<ErrorKind, u64>,
    pub status_counts: BTreeMap<u16, u64>,
    pub per_step: BTreeMap<String, StepStats>,
    /// One point per completed second.
    pub series: Vec<Point>,
    window: StepStats,
    /// Most recent failures, oldest first.
    pub errors: VecDeque<RequestRecord>,
    /// Detailed per-request records; only kept when enabled.
    pub keep_records: bool,
    pub records: Vec<RequestRecord>,
    pub dropped_records: u64,
}

impl Metrics {
    pub fn new(keep_records: bool) -> Self {
        Metrics {
            keep_records,
            ..Default::default()
        }
    }

    pub fn add(&mut self, rec: RequestRecord) {
        // Attribute to the second in which the request finished.
        self.tick((rec.at + rec.duration).as_secs());
        self.requests += 1;
        self.window.requests += 1;
        self.latency.add(rec.duration);
        self.window.latency.add(rec.duration);
        if let Some(s) = rec.status {
            *self.status_counts.entry(s).or_default() += 1;
        }
        let step = self.per_step.entry(rec.step.clone()).or_default();
        if step.requests == 0 {
            step.method = rec.method.as_str();
            step.path = rec.path.clone();
        }
        step.requests += 1;
        step.latency.add(rec.duration);
        if let Some(kind) = rec.error {
            self.failures += 1;
            self.window.failures += 1;
            step.failures += 1;
            *self.by_kind.entry(kind).or_default() += 1;
            if self.errors.len() == MAX_ERRORS {
                self.errors.pop_front();
            }
            self.errors.push_back(rec.clone());
        }
        if self.keep_records {
            if self.records.len() < MAX_RECORDS {
                self.records.push(rec);
            } else {
                self.dropped_records += 1;
            }
        }
    }

    /// Closes every one-second window that ended before `now_secs`.
    /// Call periodically so the series advances even without traffic.
    pub fn tick(&mut self, now_secs: u64) {
        while (self.series.len() as u64) < now_secs && self.series.len() < MAX_SERIES {
            let w = std::mem::take(&mut self.window);
            self.series.push(Point {
                t: self.series.len() as u64 + 1,
                requests: w.requests,
                failures: w.failures,
                p50: w.latency.percentile(0.50),
                p95: w.latency.percentile(0.95),
                p99: w.latency.percentile(0.99),
            });
        }
    }

    /// Requests in the last completed second.
    pub fn current_rps(&self) -> u64 {
        self.series.last().map_or(0, |p| p.requests)
    }

    pub fn error_rate(&self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.failures as f64 / self.requests as f64
        }
    }

    pub fn kind_count(&self, kind: ErrorKind) -> u64 {
        self.by_kind.get(&kind).copied().unwrap_or(0)
    }
}

fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}

fn to_csv(rows: impl IntoIterator<Item = Vec<String>>) -> String {
    let mut w = csv::Writer::from_writer(Vec::new());
    for r in rows {
        w.write_record(&r).expect("write to Vec");
    }
    String::from_utf8(w.into_inner().expect("flush Vec")).expect("utf8")
}

/// Aggregated results: one row for the whole test, one per step.
pub fn summary_csv(m: &Metrics, runtime: Duration) -> String {
    let secs = runtime.as_secs_f64().max(0.001);
    let row = |name: &str, requests: u64, failures: u64, h: &Histogram, statuses: String| {
        let rate = if requests == 0 {
            0.0
        } else {
            failures as f64 / requests as f64
        };
        vec![
            name.to_string(),
            requests.to_string(),
            failures.to_string(),
            format!("{:.4}", rate),
            format!("{:.2}", requests as f64 / secs),
            ms(h.mean()),
            ms(h.percentile(0.50)),
            ms(h.percentile(0.95)),
            ms(h.percentile(0.99)),
            statuses,
        ]
    };
    let header = [
        "step",
        "requests",
        "failures",
        "error_rate",
        "rps",
        "mean_ms",
        "p50_ms",
        "p95_ms",
        "p99_ms",
        "status_counts",
    ];
    let statuses = m
        .status_counts
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let kinds = m
        .by_kind
        .iter()
        .map(|(k, v)| format!("{}:{v}", k.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let mut rows = vec![
        header.map(String::from).to_vec(),
        row(
            "(total)",
            m.requests,
            m.failures,
            &m.latency,
            format!("{statuses} {kinds}").trim().to_string(),
        ),
    ];
    rows.extend(
        m.per_step
            .iter()
            .map(|(name, s)| row(name, s.requests, s.failures, &s.latency, String::new())),
    );
    to_csv(rows)
}

/// Kept request records. Contains no messages, headers, bodies or query strings.
pub fn records_csv(m: &Metrics) -> String {
    let header = [
        "at_ms",
        "vu",
        "step",
        "method",
        "path",
        "status",
        "duration_ms",
        "error",
    ];
    let rows = m.records.iter().map(|r| {
        vec![
            r.at.as_millis().to_string(),
            r.vu.to_string(),
            r.step.clone(),
            r.method.as_str().to_string(),
            r.path.clone(),
            r.status.map(|s| s.to_string()).unwrap_or_default(),
            ms(r.duration),
            r.error.map(|e| e.as_str().to_string()).unwrap_or_default(),
        ]
    });
    to_csv(std::iter::once(header.map(String::from).to_vec()).chain(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Method;

    fn rec(at_ms: u64, dur_ms: u64, error: Option<ErrorKind>) -> RequestRecord {
        RequestRecord {
            at: Duration::from_millis(at_ms),
            vu: 0,
            step: "s/a".into(),
            method: Method::Get,
            path: "/".into(),
            status: Some(if error.is_some() { 500 } else { 200 }),
            duration: Duration::from_millis(dur_ms),
            error,
            message: String::new(),
        }
    }

    fn close(actual: Duration, expected_ms: f64) -> bool {
        let a = actual.as_secs_f64() * 1000.0;
        (a - expected_ms).abs() <= expected_ms * 0.05
    }

    #[test]
    fn percentiles_of_uniform_distribution() {
        let mut h = Histogram::default();
        for ms in 1..=1000 {
            h.add(Duration::from_millis(ms));
        }
        assert!(close(h.percentile(0.50), 500.0), "{:?}", h.percentile(0.5));
        assert!(close(h.percentile(0.95), 950.0), "{:?}", h.percentile(0.95));
        assert!(close(h.percentile(0.99), 990.0), "{:?}", h.percentile(0.99));
        assert!(close(h.mean(), 500.5));
    }

    #[test]
    fn percentiles_not_derived_from_mean() {
        let mut h = Histogram::default();
        for _ in 0..98 {
            h.add(Duration::from_millis(10));
        }
        h.add(Duration::from_secs(5));
        h.add(Duration::from_secs(5));
        assert!(close(h.percentile(0.50), 10.0));
        assert!(close(h.percentile(0.99), 5000.0));
        assert!(h.mean() > Duration::from_millis(100));
    }

    #[test]
    fn histogram_clamps_extremes() {
        let mut h = Histogram::default();
        h.add(Duration::ZERO);
        h.add(Duration::from_secs(3600));
        assert!(h.percentile(0.0) < Duration::from_millis(1));
        assert!(h.percentile(1.0) <= Duration::from_secs(60));
        assert_eq!(Histogram::default().percentile(0.5), Duration::ZERO);
    }

    #[test]
    fn aggregates_and_windows() {
        let mut m = Metrics::new(false);
        for i in 0..10 {
            m.add(rec(i * 100, 10, None)); // 10 requests in second 0
        }
        m.add(rec(1500, 10, Some(ErrorKind::Status))); // second 1
        m.tick(2);
        assert_eq!((m.requests, m.failures), (11, 1));
        assert_eq!(m.series.len(), 2);
        assert_eq!((m.series[0].requests, m.series[0].failures), (10, 0));
        assert_eq!((m.series[1].requests, m.series[1].failures), (1, 1));
        assert_eq!(m.current_rps(), 1);
        assert_eq!(m.status_counts[&200], 10);
        assert_eq!(m.kind_count(ErrorKind::Status), 1);
        assert_eq!(m.per_step["s/a"].requests, 11);
        assert!((m.error_rate() - 1.0 / 11.0).abs() < 1e-9);
        m.tick(5); // idle seconds still produce points
        assert_eq!(m.series.len(), 5);
        assert_eq!(m.current_rps(), 0);
    }

    #[test]
    fn csv_escapes_and_omits_messages() {
        let mut m = Metrics::new(true);
        let mut r = rec(0, 10, Some(ErrorKind::Other));
        r.step = "a,\"b\"\nc".into();
        r.message = "secret-token".into();
        m.add(r);
        m.add(rec(5, 20, None));
        let s = summary_csv(&m, Duration::from_secs(2));
        assert!(s.starts_with("step,requests,failures,"));
        assert!(s.contains("\"a,\"\"b\"\"\nc\""), "{s}");
        assert!(s.contains("(total),2,1,0.5000,1.00,"), "{s}");
        assert!(s.contains("200:1 500:1 other:1"), "{s}");
        let d = records_csv(&m);
        assert_eq!(d.lines().count(), 1 + 3); // header + 2 records, one spanning 2 lines
        assert!(!d.contains("secret-token") && !s.contains("secret-token"));
        let mut rdr = csv::Reader::from_reader(d.as_bytes());
        let first = rdr.records().next().unwrap().unwrap();
        assert_eq!(&first[2], "a,\"b\"\nc");
        assert_eq!(&first[7], "other");
    }

    #[test]
    fn collections_are_bounded() {
        let mut m = Metrics::new(true);
        for _ in 0..MAX_RECORDS + 5 {
            m.add(rec(0, 1, Some(ErrorKind::Timeout)));
        }
        assert_eq!(m.errors.len(), MAX_ERRORS);
        assert_eq!(m.records.len(), MAX_RECORDS);
        assert_eq!(m.dropped_records, 5);
        m.tick(u64::MAX);
        assert_eq!(m.series.len(), MAX_SERIES);
        assert!(Metrics::new(false).records.is_empty());
    }
}
