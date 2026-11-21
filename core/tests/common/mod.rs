#![allow(dead_code)]

// Each integration-test file compiles this module separately and may use only some helpers.

use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tempfile::TempDir;

/// Generates `len` deterministic pseudo-random bytes from `seed` using a hand-written
/// SplitMix64 algorithm.
///
/// Unlike plain xorshift, SplitMix64 does not get stuck when `seed == 0` because it uses
/// a Weyl sequence increment (`+ 0x9e3779b97f4a7c15`).
pub fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut bytes = Vec::with_capacity(len);

    while bytes.len() < len {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;

        let chunk = z.to_le_bytes();
        let needed = (len - bytes.len()).min(8);
        bytes.extend_from_slice(&chunk[..needed]);
    }

    bytes
}

/// Creates a new temporary directory for integration tests.
///
/// Callers must retain the returned [`TempDir`] guard for the entire duration of the test.
pub fn make_temp_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temporary test directory")
}

/// A reader wrapper that injects `ErrorKind::Interrupted` errors before successful reads.
pub struct InterruptingReader {
    inner: Cursor<Vec<u8>>,
    interrupts_remaining: usize,
    pub total_interrupts_injected: usize,
}

impl InterruptingReader {
    pub fn new(data: Vec<u8>, interrupts: usize) -> Self {
        Self {
            inner: Cursor::new(data),
            interrupts_remaining: interrupts,
            total_interrupts_injected: 0,
        }
    }
}

impl Read for InterruptingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.interrupts_remaining > 0 {
            self.interrupts_remaining -= 1;
            self.total_interrupts_injected += 1;
            return Err(std::io::Error::new(
                ErrorKind::Interrupted,
                "simulated interrupted read",
            ));
        }
        self.inner.read(buf)
    }
}

impl Seek for InterruptingReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A reader wrapper that simulates source file growth by returning data beyond expected_size.
pub struct GrowingReader {
    inner: Cursor<Vec<u8>>,
}

impl GrowingReader {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
        }
    }
}

impl Read for GrowingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for GrowingReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A reader wrapper that ends prematurely before yielding expected_size bytes.
pub struct EarlyEofReader {
    inner: Cursor<Vec<u8>>,
}

impl EarlyEofReader {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
        }
    }
}

impl Read for EarlyEofReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for EarlyEofReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A reader wrapper that sets a cancellation token on its N-th `read()` call (1-based index).
pub struct CancellingOnNthReadReader {
    inner: Cursor<Vec<u8>>,
    target_read: usize,
    current_read: usize,
    token: Arc<AtomicBool>,
}

impl CancellingOnNthReadReader {
    pub fn new(data: Vec<u8>, target_read: usize, token: Arc<AtomicBool>) -> Self {
        Self {
            inner: Cursor::new(data),
            target_read,
            current_read: 0,
            token,
        }
    }
}

impl Read for CancellingOnNthReadReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.current_read += 1;
        if self.current_read == self.target_read {
            self.token.store(true, Ordering::Relaxed);
        }
        self.inner.read(buf)
    }
}

impl Seek for CancellingOnNthReadReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A reader wrapper that yields at most `max_per_read` bytes per `read()` call.
pub struct ShortReadReader {
    inner: Cursor<Vec<u8>>,
    max_per_read: usize,
}

impl ShortReadReader {
    pub fn new(data: Vec<u8>, max_per_read: usize) -> Self {
        Self {
            inner: Cursor::new(data),
            max_per_read,
        }
    }
}

impl Read for ShortReadReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let to_read = buf.len().min(self.max_per_read);
        self.inner.read(&mut buf[..to_read])
    }
}

impl Seek for ShortReadReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A reader wrapper that fails with an I/O error on its N-th `read()` call (1-based index).
pub struct FailingReader {
    inner: Cursor<Vec<u8>>,
    target_read: usize,
    current_read: usize,
    error: Option<std::io::Error>,
}

impl FailingReader {
    pub fn new(data: Vec<u8>, target_read: usize, error: std::io::Error) -> Self {
        Self {
            inner: Cursor::new(data),
            target_read,
            current_read: 0,
            error: Some(error),
        }
    }
}

impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.current_read += 1;
        if self.current_read == self.target_read {
            if let Some(err) = self.error.take() {
                return Err(err);
            }
        }
        self.inner.read(buf)
    }
}

impl Seek for FailingReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Reusable bounded reader wrapping any `Read` type that enforces a hard call bound.
pub struct BoundedReader<R> {
    inner: R,
    calls: usize,
    max_calls: usize,
    shared_calls: Option<Arc<AtomicUsize>>,
}

impl<R: Read> BoundedReader<R> {
    pub fn new(inner: R, max_calls: usize) -> Self {
        Self {
            inner,
            calls: 0,
            max_calls,
            shared_calls: None,
        }
    }

    pub fn with_shared_calls(inner: R, max_calls: usize, shared_calls: Arc<AtomicUsize>) -> Self {
        Self {
            inner,
            calls: 0,
            max_calls,
            shared_calls: Some(shared_calls),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if let Some(ref sc) = self.shared_calls {
            sc.fetch_add(1, Ordering::SeqCst);
        }
        if self.calls > self.max_calls {
            panic!(
                "BoundedReader call limit exceeded: {} > {}",
                self.calls, self.max_calls
            );
        }
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for BoundedReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}
