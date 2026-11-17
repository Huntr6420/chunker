mod common;

use std::collections::HashSet;
use std::io::{Cursor, ErrorKind, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chunker_core::stream::{is_cancelled, read_retrying_unstable};

struct InterruptThenBytesReader {
    interrupts_remaining: usize,
    bytes: Vec<u8>,
    calls: usize,
}

impl InterruptThenBytesReader {
    fn new(interrupts: usize, data: &[u8]) -> Self {
        Self {
            interrupts_remaining: interrupts,
            bytes: data.to_vec(),
            calls: 0,
        }
    }
}

impl Read for InterruptThenBytesReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.interrupts_remaining > 0 {
            self.interrupts_remaining -= 1;
            return Err(std::io::Error::new(
                ErrorKind::Interrupted,
                "operation interrupted",
            ));
        }
        let n = buf.len().min(self.bytes.len());
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes.drain(..n);
        Ok(n)
    }
}

struct FailReader {
    calls: usize,
    kind: ErrorKind,
    msg: &'static str,
}

impl FailReader {
    fn new(kind: ErrorKind, msg: &'static str) -> Self {
        Self {
            calls: 0,
            kind,
            msg,
        }
    }
}

impl Read for FailReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        Err(std::io::Error::new(self.kind, self.msg))
    }
}

struct InterruptedThenFailReader {
    interrupts_remaining: usize,
    calls: usize,
    kind: ErrorKind,
    msg: &'static str,
}

impl InterruptedThenFailReader {
    fn new(interrupts: usize, kind: ErrorKind, msg: &'static str) -> Self {
        Self {
            interrupts_remaining: interrupts,
            calls: 0,
            kind,
            msg,
        }
    }
}

impl Read for InterruptedThenFailReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.interrupts_remaining > 0 {
            self.interrupts_remaining -= 1;
            return Err(std::io::Error::new(
                ErrorKind::Interrupted,
                "interrupted before failure",
            ));
        }
        Err(std::io::Error::new(self.kind, self.msg))
    }
}

#[test]
fn test_read_retrying_interrupted_once_then_bytes() {
    let mut reader = InterruptThenBytesReader::new(1, b"hello world");
    let mut buf = [0u8; 32];
    let n = read_retrying_unstable(&mut reader, &mut buf).expect("read should succeed after retry");
    assert_eq!(n, 11);
    assert_eq!(&buf[..n], b"hello world");
    assert_eq!(reader.calls, 2);
}

#[test]
fn test_read_retrying_interrupted_several_times_then_bytes() {
    let mut reader = InterruptThenBytesReader::new(5, b"streaming data payload");
    let mut buf = [0u8; 64];
    let n =
        read_retrying_unstable(&mut reader, &mut buf).expect("read should succeed after retries");
    assert_eq!(n, 22);
    assert_eq!(&buf[..n], b"streaming data payload");
    assert_eq!(reader.calls, 6);
}

#[test]
fn test_read_retrying_non_interrupted_error_no_retry() {
    let mut reader = FailReader::new(ErrorKind::PermissionDenied, "access denied to device");
    let mut buf = [0u8; 16];
    let err = read_retrying_unstable(&mut reader, &mut buf)
        .expect_err("should return non-interrupted error");
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert_eq!(err.to_string(), "access denied to device");
    assert_eq!(
        reader.calls, 1,
        "reader must be called exactly once without retry"
    );

    let mut reader_with_interrupted =
        InterruptedThenFailReader::new(2, ErrorKind::UnexpectedEof, "stream cut off abruptly");
    let err2 = read_retrying_unstable(&mut reader_with_interrupted, &mut buf)
        .expect_err("should return unexpected eof");
    assert_eq!(err2.kind(), ErrorKind::UnexpectedEof);
    assert_eq!(err2.to_string(), "stream cut off abruptly");
    assert_eq!(
        reader_with_interrupted.calls, 3,
        "retried on Interrupted twice, called once for failure, and not retried after it"
    );
}

#[test]
fn test_read_retrying_exhausted_reader_returns_ok_zero() {
    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut buf = [0u8; 16];
    let n =
        read_retrying_unstable(&mut reader, &mut buf).expect("exhausted reader must return Ok(0)");
    assert_eq!(n, 0);

    let mut non_empty_reader = Cursor::new(vec![1, 2, 3]);
    let mut empty_buf = [0u8; 0];
    let n_empty = read_retrying_unstable(&mut non_empty_reader, &mut empty_buf)
        .expect("empty buffer must return Ok(0)");
    assert_eq!(n_empty, 0);
}

#[test]
fn test_read_retrying_partial_read() {
    let mut reader = Cursor::new(vec![10, 20, 30, 40]);
    let mut buf = [0u8; 16];
    let n = read_retrying_unstable(&mut reader, &mut buf).expect("partial read returns Ok(n)");
    assert_eq!(n, 4);
    assert!(n < buf.len());
    assert_eq!(&buf[..n], &[10, 20, 30, 40]);
}

#[test]
fn test_read_retrying_through_dyn_read() {
    let mut data: &[u8] = b"dyn reader test bytes";
    let dyn_reader: &mut dyn Read = &mut data;
    let mut buf = [0u8; 32];
    let n = read_retrying_unstable(dyn_reader, &mut buf).expect("read through &mut dyn Read");
    assert_eq!(n, 21);
    assert_eq!(&buf[..n], b"dyn reader test bytes");
}

#[test]
fn test_is_cancelled() {
    // None -> false
    assert!(!is_cancelled(None));

    // unset flag -> false
    let flag = Arc::new(AtomicBool::new(false));
    assert!(!is_cancelled(Some(&flag)));

    // set flag -> true
    flag.store(true, Ordering::Relaxed);
    assert!(is_cancelled(Some(&flag)));
}

#[test]
fn test_helper_seeded_bytes() {
    // same seed and length give identical bytes
    let a = common::seeded_bytes(12345, 100);
    let b = common::seeded_bytes(12345, 100);
    assert_eq!(a, b);

    // different seeds differ
    let c = common::seeded_bytes(54321, 100);
    assert_ne!(a, c);

    // length 0 is empty
    assert_eq!(common::seeded_bytes(12345, 0), Vec::<u8>::new());
    assert!(common::seeded_bytes(12345, 0).is_empty());

    // a shorter output is a prefix of a longer one for the same seed
    let short = common::seeded_bytes(42, 64);
    let long = common::seeded_bytes(42, 128);
    assert_eq!(&long[..64], short.as_slice());

    // seed 0 is not constant
    let s0 = common::seeded_bytes(0, 4096);
    let first_byte = s0[0];
    assert!(
        s0.iter().any(|&byte| byte != first_byte),
        "seed 0 output must not be constant"
    );

    // 4096 bytes are not periodic at any shift 1..=64
    for shift in 1..=64 {
        assert_ne!(
            &s0[..4096 - shift],
            &s0[shift..4096],
            "seed 0 output is periodic at shift {}",
            shift
        );
    }
    let s99 = common::seeded_bytes(99, 4096);
    for shift in 1..=64 {
        assert_ne!(
            &s99[..4096 - shift],
            &s99[shift..4096],
            "seed 99 output is periodic at shift {}",
            shift
        );
    }

    // at least 200 distinct byte values in 4096 bytes
    let distinct_s0: HashSet<u8> = s0.iter().copied().collect();
    assert!(
        distinct_s0.len() >= 200,
        "seed 0 produced only {} distinct byte values (< 200)",
        distinct_s0.len()
    );
    let distinct_s99: HashSet<u8> = s99.iter().copied().collect();
    assert!(
        distinct_s99.len() >= 200,
        "seed 99 produced only {} distinct byte values (< 200)",
        distinct_s99.len()
    );
}

#[test]
fn test_helper_make_temp_dir() {
    let path1;
    let path2;
    {
        let dir1 = common::make_temp_dir();
        let dir2 = common::make_temp_dir();
        path1 = dir1.path().to_path_buf();
        path2 = dir2.path().to_path_buf();

        // the directory exists
        assert!(path1.exists(), "dir1 must exist while guard is held");
        assert!(path2.exists(), "dir2 must exist while guard is held");

        // two calls give different paths
        assert_ne!(path1, path2, "two calls must produce different paths");
    }

    // the directory is gone after the guard is dropped
    assert!(!path1.exists(), "dir1 must be gone after guard is dropped");
    assert!(!path2.exists(), "dir2 must be gone after guard is dropped");
}
