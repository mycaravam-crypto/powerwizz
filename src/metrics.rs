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
