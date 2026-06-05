use std::time::Duration;

use hdrhistogram::Histogram as HdrHistogram;
use serde::{Deserialize, Serialize};

const LOW_NANOS: u64 = 1;
/// 60s ceiling; larger values are clamped via `saturating_record`.
const HIGH_NANOS: u64 = 60_000_000_000;
/// 3 significant figures ⇒ ~0.1% relative error.
const SIGNIFICANT_FIGURES: u8 = 3;

/// A latency recorder built on [HdrHistogram](https://hdrhistogram.github.io/HdrHistogram/)
/// — the standard for high-dynamic-range latency with bounded memory and O(1)
/// recording. Thin wrapper: records nanoseconds clamped to `[1ns, 60s]` and
/// exposes the p50/p95/p99 figures the dashboard graphs.
#[derive(Debug, Clone)]
pub struct LatencyHistogram {
    inner: HdrHistogram<u64>,
}

/// A serialisable point-in-time view of a [`LatencyHistogram`], sent to the
/// dashboard over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencySnapshot {
    pub count: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyHistogram {
    pub fn new() -> Self {
        let inner = HdrHistogram::new_with_bounds(LOW_NANOS, HIGH_NANOS, SIGNIFICANT_FIGURES)
            .expect("static HdrHistogram bounds are valid");
        Self { inner }
    }

    pub fn record_nanos(&mut self, nanos: u64) {
        self.inner.saturating_record(nanos.max(LOW_NANOS));
    }

    pub fn record(&mut self, latency: Duration) {
        let nanos = u64::try_from(latency.as_nanos()).unwrap_or(u64::MAX);
        self.record_nanos(nanos);
    }

    pub fn count(&self) -> u64 {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    pub fn min_ns(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            self.inner.min()
        }
    }

    pub fn max_ns(&self) -> u64 {
        self.inner.max()
    }

    pub fn mean_ns(&self) -> f64 {
        self.inner.mean()
    }

    /// Value at `percentile` (given in `[0, 100]`, clamped) in nanoseconds.
    pub fn percentile_ns(&self, percentile: f64) -> u64 {
        let quantile = (percentile / 100.0).clamp(0.0, 1.0);
        self.inner.value_at_quantile(quantile)
    }

    pub fn p50_ns(&self) -> u64 {
        self.percentile_ns(50.0)
    }

    pub fn p95_ns(&self) -> u64 {
        self.percentile_ns(95.0)
    }

    pub fn p99_ns(&self) -> u64 {
        self.percentile_ns(99.0)
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }

    pub fn snapshot(&self) -> LatencySnapshot {
        LatencySnapshot {
            count: self.count(),
            min_ns: self.min_ns(),
            max_ns: self.max_ns(),
            mean_ns: self.mean_ns(),
            p50_ns: self.p50_ns(),
            p95_ns: self.p95_ns(),
            p99_ns: self.p99_ns(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_histogram_reports_zeroes() {
        let h = LatencyHistogram::new();
        assert!(h.is_empty());
        assert_eq!(h.count(), 0);
        assert_eq!(h.min_ns(), 0);
        assert_eq!(h.max_ns(), 0);
        assert_eq!(h.mean_ns(), 0.0);
        let snap = h.snapshot();
        assert_eq!(snap.count, 0);
        assert_eq!(snap.p99_ns, 0);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(
            LatencyHistogram::default().count(),
            LatencyHistogram::new().count()
        );
    }

    #[test]
    fn records_and_counts() {
        let mut h = LatencyHistogram::new();
        for ns in [100, 200, 300] {
            h.record_nanos(ns);
        }
        assert_eq!(h.count(), 3);
        assert!(!h.is_empty());
    }

    #[test]
    fn zero_is_clamped_to_one_ns() {
        let mut h = LatencyHistogram::new();
        h.record_nanos(0);
        assert_eq!(h.count(), 1);
        assert_eq!(h.min_ns(), 1);
    }

    #[test]
    fn values_above_ceiling_are_clamped() {
        let mut h = LatencyHistogram::new();
        h.record_nanos(u64::MAX);
        assert_eq!(h.count(), 1);
        // Clamped to the 60s ceiling, within HdrHistogram's bucket precision
        // (the reported max may round slightly above the configured high).
        let max = h.max_ns();
        let diff = (max as i64 - HIGH_NANOS as i64).unsigned_abs();
        assert!(diff <= HIGH_NANOS / 100, "max was {max}");
    }

    #[test]
    fn records_duration() {
        let mut h = LatencyHistogram::new();
        h.record(Duration::from_micros(500));
        // 500µs == 500_000ns, within precision.
        assert!((h.min_ns() as i64 - 500_000).abs() < 1_000);
    }

    #[test]
    fn record_duration_overflowing_u64_nanos_is_clamped() {
        let mut h = LatencyHistogram::new();
        // Duration::MAX has more nanoseconds than u64 can hold; the conversion
        // saturates to u64::MAX, then clamps to the 60s ceiling.
        h.record(Duration::MAX);
        assert_eq!(h.count(), 1);
        let diff = (h.max_ns() as i64 - HIGH_NANOS as i64).unsigned_abs();
        assert!(diff <= HIGH_NANOS / 100);
    }

    #[test]
    fn percentiles_are_ordered_and_plausible() {
        let mut h = LatencyHistogram::new();
        for ns in 1..=1000 {
            h.record_nanos(ns);
        }
        let p50 = h.p50_ns();
        let p95 = h.p95_ns();
        let p99 = h.p99_ns();
        assert!(p50 <= p95 && p95 <= p99);
        // Median of 1..=1000 is ~500, within 1% precision tolerance.
        assert!((450..=550).contains(&p50), "p50 was {p50}");
        assert!((940..=1000).contains(&p95), "p95 was {p95}");
    }

    #[test]
    fn percentile_clamps_out_of_range_inputs() {
        let mut h = LatencyHistogram::new();
        for ns in 1..=1000 {
            h.record_nanos(ns);
        }
        // Out-of-range percentiles clamp to the 0% / 100% boundaries.
        assert_eq!(h.percentile_ns(200.0), h.percentile_ns(100.0));
        assert_eq!(h.percentile_ns(-50.0), h.percentile_ns(0.0));
    }

    #[test]
    fn clear_empties() {
        let mut h = LatencyHistogram::new();
        h.record_nanos(10);
        h.clear();
        assert!(h.is_empty());
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let mut h = LatencyHistogram::new();
        for ns in [10, 20, 30, 40, 50] {
            h.record_nanos(ns);
        }
        let snap = h.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back: LatencySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
        assert_eq!(back.count, 5);
    }
}
