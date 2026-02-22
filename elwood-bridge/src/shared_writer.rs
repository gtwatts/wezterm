//! Swappable writer for the virtual terminal.
//!
//! The `SharedWriter` wraps an `Arc<Mutex<Box<dyn Write + Send>>>` so that
//! multiple owners (Terminal, ElwoodPane) can hold a reference to the same
//! writer, and the underlying destination can be swapped at runtime.
//!
//! This is the key mechanism that enables PTY embedding: the Terminal is
//! created once with a SharedWriter pointing at `io::sink()`, and when a
//! PTY is spawned the inner writer is swapped to the PTY master's writer.
//! When the PTY exits (or the user switches back to agent mode), the inner
//! writer is swapped back to `io::sink()`.
//!
//! Because `Terminal::key_down()` encodes keystrokes and writes them to its
//! internal writer, swapping the SharedWriter destination transparently
//! routes keystrokes to the correct target without recreating the Terminal.

use parking_lot::Mutex;
use std::io::{self, Write};
use std::sync::Arc;

/// A `Write` implementation backed by a swappable inner writer.
///
/// Created with `io::sink()` as the initial destination. Call [`swap`] to
/// redirect writes to a PTY master writer, and [`swap_to_sink`] to reset.
///
/// # Clone
///
/// `SharedWriter` is cheaply cloneable (Arc-based). The Terminal holds one
/// clone; ElwoodPane holds another for swapping.
///
/// # Example
///
/// ```
/// use elwood_bridge::shared_writer::SharedWriter;
/// use std::io::Write;
///
/// let writer = SharedWriter::new();
/// // Initially writes go to sink (discarded)
/// let mut w = writer.clone();
/// w.write_all(b"ignored").unwrap();
///
/// // Swap to a real destination
/// let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
/// let buf2 = buf.clone();
/// writer.swap(Box::new(VecWriter(buf.clone())));
/// w.write_all(b"hello").unwrap();
/// assert_eq!(&*buf2.lock().unwrap(), b"hello");
///
/// // Swap back to sink
/// writer.swap_to_sink();
/// w.write_all(b"gone").unwrap();
/// assert_eq!(&*buf2.lock().unwrap(), b"hello"); // unchanged
///
/// // Helper for the doctest
/// struct VecWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
/// impl std::io::Write for VecWriter {
///     fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
///         self.0.lock().unwrap().extend_from_slice(buf);
///         Ok(buf.len())
///     }
///     fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
/// }
/// ```
#[derive(Clone)]
pub struct SharedWriter {
    inner: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl SharedWriter {
    /// Create a new `SharedWriter` that initially discards all writes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Box::new(io::sink()))),
        }
    }

    /// Swap the inner writer to `new_writer`.
    ///
    /// All subsequent writes (including from Terminal::key_down()) will go
    /// to the new destination. The old writer is dropped.
    pub fn swap(&self, new_writer: Box<dyn Write + Send>) {
        *self.inner.lock() = new_writer;
    }

    /// Reset the inner writer to `io::sink()`, discarding all writes.
    pub fn swap_to_sink(&self) {
        *self.inner.lock() = Box::new(io::sink());
    }
}

impl Default for SharedWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.lock().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.lock().flush()
    }
}

impl std::fmt::Debug for SharedWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedWriter").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    /// Helper writer that collects bytes into a shared Vec.
    struct CollectWriter(StdArc<StdMutex<Vec<u8>>>);

    impl Write for CollectWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_new_writes_to_sink() {
        let mut w = SharedWriter::new();
        // Writing to sink should succeed and discard data
        assert_eq!(w.write(b"hello").unwrap(), 5);
        w.flush().unwrap();
    }

    #[test]
    fn test_swap_redirects_writes() {
        let shared = SharedWriter::new();
        let mut w = shared.clone();

        let buf = StdArc::new(StdMutex::new(Vec::<u8>::new()));
        shared.swap(Box::new(CollectWriter(buf.clone())));

        w.write_all(b"hello pty").unwrap();
        w.flush().unwrap();

        assert_eq!(&*buf.lock().unwrap(), b"hello pty");
    }

    #[test]
    fn test_swap_to_sink_resets() {
        let shared = SharedWriter::new();
        let mut w = shared.clone();

        let buf = StdArc::new(StdMutex::new(Vec::<u8>::new()));
        shared.swap(Box::new(CollectWriter(buf.clone())));

        w.write_all(b"before").unwrap();
        shared.swap_to_sink();
        w.write_all(b"after").unwrap();

        // Only "before" should be captured
        assert_eq!(&*buf.lock().unwrap(), b"before");
    }

    #[test]
    fn test_clone_shares_same_writer() {
        let shared = SharedWriter::new();
        let mut w1 = shared.clone();
        let mut w2 = shared.clone();

        let buf = StdArc::new(StdMutex::new(Vec::<u8>::new()));
        shared.swap(Box::new(CollectWriter(buf.clone())));

        w1.write_all(b"from_w1_").unwrap();
        w2.write_all(b"from_w2").unwrap();

        assert_eq!(&*buf.lock().unwrap(), b"from_w1_from_w2");
    }

    #[test]
    fn test_multiple_swaps() {
        let shared = SharedWriter::new();
        let mut w = shared.clone();

        let buf1 = StdArc::new(StdMutex::new(Vec::<u8>::new()));
        let buf2 = StdArc::new(StdMutex::new(Vec::<u8>::new()));

        shared.swap(Box::new(CollectWriter(buf1.clone())));
        w.write_all(b"first").unwrap();

        shared.swap(Box::new(CollectWriter(buf2.clone())));
        w.write_all(b"second").unwrap();

        assert_eq!(&*buf1.lock().unwrap(), b"first");
        assert_eq!(&*buf2.lock().unwrap(), b"second");
    }

    #[test]
    fn test_default_is_sink() {
        let mut w = SharedWriter::default();
        assert_eq!(w.write(b"test").unwrap(), 4);
    }
}
