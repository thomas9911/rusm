use std::sync::atomic::{AtomicU64, Ordering};

/// A thread-safe counter backed by an [`AtomicU64`].
///
/// One relaxed atomic add per event is the entire cost of instrumentation: the
/// observer samples counters on its own cadence rather than taking an event per
/// operation, which is why the live observer stays cheap. `Relaxed` is correct
/// here as counters carry no happens-before relationship with other memory.
#[derive(Debug, Default)]
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    pub const fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    pub const fn with_value(initial: u64) -> Self {
        Self {
            value: AtomicU64::new(initial),
        }
    }

    pub fn incr(&self) -> u64 {
        self.add(1)
    }

    pub fn add(&self, n: u64) -> u64 {
        self.value.fetch_add(n, Ordering::Relaxed)
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn set(&self, n: u64) {
        self.value.store(n, Ordering::Relaxed);
    }

    pub fn reset(&self) -> u64 {
        self.value.swap(0, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn new_starts_at_zero() {
        assert_eq!(Counter::new().get(), 0);
        assert_eq!(Counter::default().get(), 0);
    }

    #[test]
    fn with_value_starts_at_initial() {
        assert_eq!(Counter::with_value(42).get(), 42);
    }

    #[test]
    fn incr_returns_previous_and_advances() {
        let c = Counter::new();
        assert_eq!(c.incr(), 0);
        assert_eq!(c.incr(), 1);
        assert_eq!(c.get(), 2);
    }

    #[test]
    fn add_returns_previous() {
        let c = Counter::with_value(10);
        assert_eq!(c.add(5), 10);
        assert_eq!(c.get(), 15);
    }

    #[test]
    fn set_overwrites() {
        let c = Counter::with_value(10);
        c.set(99);
        assert_eq!(c.get(), 99);
    }

    #[test]
    fn reset_returns_old_value_and_zeroes() {
        let c = Counter::with_value(7);
        assert_eq!(c.reset(), 7);
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn concurrent_increments_are_lossless() {
        let c = Arc::new(Counter::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&c);
                thread::spawn(move || {
                    for _ in 0..10_000 {
                        c.incr();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(c.get(), 8 * 10_000);
    }
}
