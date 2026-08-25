//! A wall-clock deadline around a stream.
//!
//! `TcpStream::set_read_timeout` bounds a single `read()` syscall, not an
//! exchange. Every loop iteration in a `read_exact` gets a fresh budget, so a
//! peer dribbling one byte just under the timeout keeps a thread alive
//! indefinitely — a slow-loris. With `MAX_INBOUND` handler slots, eight such
//! sockets from an unpaired machine take sync down until the app restarts.
//!
//! This wraps the stream and refuses to read or write once a real deadline has
//! passed, whatever the socket-level timeout is doing. The socket timeout stays
//! on as well: it bounds the single call we are inside when the deadline
//! expires, so the worst case is deadline + one socket timeout rather than
//! forever.
//!
//! The deadline is shared and mutable so the same wrapped stream can carry a
//! short budget through an unauthenticated handshake and a longer one once the
//! peer has proved it holds the key.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(i64::MAX)
}

/// Shared deadline, in epoch ms.
#[derive(Clone)]
pub struct Deadline(Arc<AtomicI64>);

impl Deadline {
    pub fn after(d: Duration) -> Self {
        Self(Arc::new(AtomicI64::new(now_ms() + d.as_millis() as i64)))
    }

    /// Push the deadline out. Used when an unauthenticated handshake completes
    /// and the peer has earned a longer budget.
    pub fn extend(&self, d: Duration) {
        self.0.store(now_ms() + d.as_millis() as i64, Ordering::SeqCst);
    }

    pub fn expired(&self) -> bool {
        now_ms() > self.0.load(Ordering::SeqCst)
    }

    fn check(&self) -> io::Result<()> {
        if self.expired() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "peer exceeded the time allowed for this exchange",
            ));
        }
        Ok(())
    }
}

/// A stream that stops working once its deadline passes.
pub struct Timed<S> {
    inner: S,
    deadline: Deadline,
}

impl<S> Timed<S> {
    pub fn new(inner: S, deadline: Deadline) -> Self {
        Self { inner, deadline }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S: Read> Read for Timed<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.deadline.check()?;
        self.inner.read(buf)
    }
}

impl<S: Write> Write for Timed<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.deadline.check()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Checked here too. A peer that stops reading makes flush the call that
        // blocks, so exempting it left one way to sit past the deadline.
        self.deadline.check()?;
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_are_refused_once_the_deadline_passes() {
        let d = Deadline::after(Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(5));
        let mut t = Timed::new(Cursor::new(vec![1u8, 2, 3]), d);
        let mut buf = [0u8; 3];
        let e = t.read(&mut buf).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_live_deadline_reads_normally() {
        let d = Deadline::after(Duration::from_secs(30));
        let mut t = Timed::new(Cursor::new(vec![7u8, 8]), d);
        let mut buf = [0u8; 2];
        assert_eq!(t.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, [7, 8]);
    }

    #[test]
    fn extending_revives_a_stream_for_the_authenticated_phase() {
        let d = Deadline::after(Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(5));
        assert!(d.expired());
        d.extend(Duration::from_secs(60));
        assert!(!d.expired());
        let mut t = Timed::new(Cursor::new(vec![9u8]), d);
        let mut buf = [0u8; 1];
        assert_eq!(t.read(&mut buf).unwrap(), 1);
    }

    #[test]
    fn a_dribbling_peer_cannot_outlast_the_deadline() {
        // The slow-loris shape: many small reads, each individually fine. The
        // per-read socket timeout never fires; the wall clock is what stops it.
        let d = Deadline::after(Duration::from_millis(20));
        let mut t = Timed::new(Cursor::new(vec![0u8; 4096]), d);
        let mut buf = [0u8; 1];
        let mut reads = 0;
        loop {
            match t.read(&mut buf) {
                Ok(_) => {
                    reads += 1;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => {
                    assert_eq!(e.kind(), io::ErrorKind::TimedOut);
                    break;
                }
            }
            assert!(reads < 4096, "deadline must fire before the data runs out");
        }
        assert!(reads > 0, "some progress before the deadline");
    }
}
