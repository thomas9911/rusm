/// A control signal delivered to a process.
///
/// Phase 1 has only [`Signal::Shutdown`]; later phases add messages, links, and
/// monitors — hence `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Signal {
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_is_comparable_and_debuggable() {
        assert_eq!(Signal::Shutdown, Signal::Shutdown);
        assert_eq!(format!("{:?}", Signal::Shutdown), "Shutdown");
    }
}
