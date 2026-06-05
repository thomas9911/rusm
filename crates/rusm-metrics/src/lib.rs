//! Observability primitives for RUSM: counters, latency histograms, and
//! ring-buffer time series. These feed the live observer and the benchmark
//! dashboard (`rusm-observer`, `rusm-bench`).

mod counter;
mod histogram;
mod timeseries;

pub use counter::Counter;
pub use histogram::{LatencyHistogram, LatencySnapshot};
pub use timeseries::{TimeSeries, TimeSeriesSnapshot};
