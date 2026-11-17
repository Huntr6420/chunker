use std::io::{ErrorKind, Read, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Reads bytes from `reader` into `buffer`, retrying automatically if the read fails
/// with [`ErrorKind::Interrupted`].
///
/// Returns `Ok(0)` when the reader is exhausted (EOF) or when an empty buffer is provided.
/// An empty buffer returns Ok(0), and the helper does not check cancellation.
/// Every other error is returned unchanged without retry.
#[doc(hidden)]
pub fn read_retrying_unstable<R: Read + ?Sized>(
    reader: &mut R,
    buffer: &mut [u8],
) -> Result<usize> {
    loop {
        match reader.read(buffer) {
            Ok(n) => return Ok(n),
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Checks whether an optional cancellation token has been signaled.
///
/// Returns `false` if `token` is `None` or if the flag is unset (`false`).
/// Returns `true` if `token` is `Some` and the flag is set (`true`).
/// The atomic flag is loaded using [`Ordering::Relaxed`].
pub fn is_cancelled(token: Option<&Arc<AtomicBool>>) -> bool {
    token.is_some_and(|flag| flag.load(Ordering::Relaxed))
}
