//! Byte streams carried between processes.
//!
//! A stream is a **bounded** channel of byte chunks: the reader pulls at its own
//! pace and a slow reader applies natural back-pressure to the writer (Tokio's
//! channel does this for us — no busy-poll). The read end travels in a message as
//! [`Received::Stream`](crate::Received::Stream), moving ownership to the
//! recipient exactly like every other message. This stays **Wasm-free**; the
//! `rusm-wasm` p3 bridge maps a component `stream<u8>` onto these handles.

use tokio::sync::mpsc::{channel, Receiver, Sender};

/// Default per-stream buffer (chunks) before the writer feels back-pressure.
const DEFAULT_CAPACITY: usize = 16;

/// The write end of a byte stream. Bounded: [`write`](StreamWriter::write) awaits
/// when the buffer is full, so production can't outrun a slow consumer. Dropping
/// the writer ends the stream (the reader then sees `None`).
#[derive(Clone)]
pub struct StreamWriter {
    tx: Sender<Vec<u8>>,
}

/// The read end of a byte stream, delivered as
/// [`Received::Stream`](crate::Received::Stream). Single-consumer: ownership moves
/// to the recipient, like every message.
pub struct StreamHandle {
    rx: Receiver<Vec<u8>>,
}

/// A connected `(writer, reader)` byte-stream pair with the default buffer.
pub fn stream() -> (StreamWriter, StreamHandle) {
    stream_with_capacity(DEFAULT_CAPACITY)
}

/// A connected pair with an explicit buffer depth (clamped to at least 1).
pub fn stream_with_capacity(capacity: usize) -> (StreamWriter, StreamHandle) {
    let (tx, rx) = channel(capacity.max(1));
    (StreamWriter { tx }, StreamHandle { rx })
}

impl StreamWriter {
    /// Writes one chunk, awaiting if the buffer is full (back-pressure). Returns
    /// the chunk back in `Err` if the reader has gone away.
    pub async fn write(&self, chunk: Vec<u8>) -> Result<(), Vec<u8>> {
        self.tx.send(chunk).await.map_err(|err| err.0)
    }
}

impl StreamHandle {
    /// Reads the next chunk, or `None` once the writer is dropped and the buffer
    /// drained — end of stream.
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

impl std::fmt::Debug for StreamWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamWriter").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_applies_backpressure_until_the_reader_drains() {
        let (w, mut r) = stream_with_capacity(1);
        w.write(b"1".to_vec()).await.unwrap(); // fills the single slot

        // A second write can't complete until a read frees the slot.
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(20), w.write(b"2".to_vec()))
                .await;
        assert!(
            blocked.is_err(),
            "write must block while the buffer is full"
        );

        assert_eq!(r.read().await, Some(b"1".to_vec())); // drain frees a slot
        w.write(b"2".to_vec()).await.unwrap();
        assert_eq!(r.read().await, Some(b"2".to_vec()));
    }

    #[tokio::test]
    async fn read_returns_none_after_the_writer_drops() {
        let (w, mut r) = stream();
        w.write(b"x".to_vec()).await.unwrap();
        drop(w);
        assert_eq!(r.read().await, Some(b"x".to_vec()));
        assert_eq!(r.read().await, None); // end of stream
    }

    #[tokio::test]
    async fn write_fails_once_the_reader_is_gone() {
        let (w, r) = stream();
        drop(r);
        assert_eq!(w.write(b"x".to_vec()).await, Err(b"x".to_vec()));
    }

    #[tokio::test]
    async fn a_cloned_writer_fans_into_the_same_stream() {
        use std::collections::HashSet;
        let (w, mut r) = stream();
        let w2 = w.clone();
        w.write(b"1".to_vec()).await.unwrap();
        w2.write(b"2".to_vec()).await.unwrap();
        let got: HashSet<Vec<u8>> = [r.read().await.unwrap(), r.read().await.unwrap()]
            .into_iter()
            .collect();
        assert_eq!(got, HashSet::from([b"1".to_vec(), b"2".to_vec()]));
        // Stream ends only once *every* writer is gone.
        drop(w);
        drop(w2);
        assert_eq!(r.read().await, None);
    }

    #[test]
    fn handles_format_for_debug() {
        let (w, r) = stream();
        assert!(format!("{w:?}").contains("StreamWriter"));
        assert!(format!("{r:?}").contains("StreamHandle"));
    }
}
