use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// A fixed-capacity ring buffer of recent `f64` data points, for live graphs.
///
/// The dashboard draws rolling windows (connections/sec over the last N
/// samples, say). A [`TimeSeries`] keeps only the most recent `capacity`
/// points: pushing past capacity evicts the oldest. This bounds memory no
/// matter how long a benchmark runs.
#[derive(Debug, Clone)]
pub struct TimeSeries {
    points: VecDeque<f64>,
    capacity: usize,
}

/// A serialisable view of a [`TimeSeries`], oldest point first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeSeriesSnapshot {
    pub points: Vec<f64>,
    pub capacity: usize,
}

impl TimeSeries {
    /// `capacity` of 0 is treated as 1.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            points: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, value: f64) {
        if self.points.len() == self.capacity {
            self.points.pop_front();
        }
        self.points.push_back(value);
    }

    pub fn latest(&self) -> Option<f64> {
        self.points.back().copied()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        self.points.iter().copied()
    }

    pub fn mean(&self) -> Option<f64> {
        if self.points.is_empty() {
            return None;
        }
        let sum: f64 = self.points.iter().sum();
        Some(sum / self.points.len() as f64)
    }

    pub fn max(&self) -> Option<f64> {
        self.points.iter().copied().reduce(f64::max)
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }

    pub fn snapshot(&self) -> TimeSeriesSnapshot {
        TimeSeriesSnapshot {
            points: self.points.iter().copied().collect(),
            capacity: self.capacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty_with_given_capacity() {
        let ts = TimeSeries::new(4);
        assert!(ts.is_empty());
        assert_eq!(ts.len(), 0);
        assert_eq!(ts.capacity(), 4);
        assert_eq!(ts.latest(), None);
        assert_eq!(ts.mean(), None);
        assert_eq!(ts.max(), None);
    }

    #[test]
    fn zero_capacity_becomes_one() {
        let mut ts = TimeSeries::new(0);
        assert_eq!(ts.capacity(), 1);
        ts.push(1.0);
        ts.push(2.0);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts.latest(), Some(2.0));
    }

    #[test]
    fn push_retains_latest_and_tracks_len() {
        let mut ts = TimeSeries::new(3);
        ts.push(1.0);
        ts.push(2.0);
        assert_eq!(ts.len(), 2);
        assert_eq!(ts.latest(), Some(2.0));
    }

    #[test]
    fn push_past_capacity_evicts_oldest() {
        let mut ts = TimeSeries::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            ts.push(v);
        }
        assert_eq!(ts.len(), 3);
        assert_eq!(ts.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(ts.latest(), Some(4.0));
    }

    #[test]
    fn mean_and_max_over_held_points() {
        let mut ts = TimeSeries::new(10);
        for v in [10.0, 20.0, 30.0] {
            ts.push(v);
        }
        assert_eq!(ts.mean(), Some(20.0));
        assert_eq!(ts.max(), Some(30.0));
    }

    #[test]
    fn clear_empties_but_keeps_capacity() {
        let mut ts = TimeSeries::new(5);
        ts.push(1.0);
        ts.clear();
        assert!(ts.is_empty());
        assert_eq!(ts.capacity(), 5);
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let mut ts = TimeSeries::new(3);
        ts.push(1.5);
        ts.push(2.5);
        let snap = ts.snapshot();
        assert_eq!(snap.points, vec![1.5, 2.5]);
        assert_eq!(snap.capacity, 3);
        let json = serde_json::to_string(&snap).unwrap();
        let back: TimeSeriesSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }
}
