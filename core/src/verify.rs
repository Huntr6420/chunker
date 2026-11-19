use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::BUFFER_SIZE;
use crate::error::ChunkerError;
use crate::manifest::{Manifest, load_manifest};
use crate::progress::{OperationKind, ProgressEvent};
use crate::stream::{is_cancelled, read_retrying_unstable};

/// Verifies a chunk bundle against its manifest on disk.
///
/// Loads and validates the manifest, then verifies each chunk's size and BLAKE3 hash,
/// and computes the overall master hash against the original file's hash in the manifest.
pub fn verify_bundle<F>(
    bundle: &Path,
    cancel_token: Option<&Arc<AtomicBool>>,
    on_progress: F,
) -> Result<(), ChunkerError>
where
    F: FnMut(ProgressEvent),
{
    // Check cancellation at entry before loading manifest
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    let manifest = load_manifest(bundle)?;

    let mut open_chunk = |index: usize| -> std::io::Result<(File, u64)> {
        let chunk_filename = format!("part-{:04}.chk", index);
        let chunk_path = bundle.join(chunk_filename);
        let file = File::open(&chunk_path)?;
        let meta = file.metadata()?;
        Ok((file, meta.len()))
    };

    verify_with_opener_unstable(&manifest, &mut open_chunk, cancel_token, on_progress)
}

/// Non-stable integration-test seam for verification.
///
/// Accepts a validated [`Manifest`], a closure to open each chunk by zero-based index
/// returning `(reader, metadata_len)`, a cancellation token, and a progress callback.
#[doc(hidden)]
pub fn verify_with_opener_unstable<R, O, F>(
    manifest: &Manifest,
    mut open_chunk: O,
    cancel_token: Option<&Arc<AtomicBool>>,
    mut on_progress: F,
) -> Result<(), ChunkerError>
where
    R: std::io::Read,
    O: FnMut(usize) -> std::io::Result<(R, u64)>,
    F: FnMut(ProgressEvent),
{
    // 1. Entry cancellation check
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    let mut master_hasher = blake3::Hasher::new();
    let mut total_bytes_processed = 0u64;
    let mut buffer = vec![0u8; BUFFER_SIZE];

    for (chunk_idx, chunk) in manifest.chunks.iter().enumerate() {
        let chunk_filename = format!("part-{:04}.chk", chunk_idx);

        let (mut reader, meta_len) = open_chunk(chunk_idx)?;

        // Upfront metadata size comparison inside the seam before reading or hashing
        if meta_len != chunk.size_bytes {
            return Err(ChunkerError::SizeMismatch {
                file: chunk_filename,
                expected: chunk.size_bytes,
                actual: meta_len,
            });
        }

        let mut chunk_hasher = blake3::Hasher::new();
        let mut chunk_bytes_read = 0u64;

        while chunk_bytes_read < chunk.size_bytes {
            if is_cancelled(cancel_token) {
                return Err(ChunkerError::Cancelled);
            }

            let to_read = (chunk.size_bytes - chunk_bytes_read).min(buffer.len() as u64) as usize;

            let n = read_retrying_unstable(&mut reader, &mut buffer[..to_read])?;
            if n == 0 {
                break;
            }

            chunk_hasher.update(&buffer[..n]);
            master_hasher.update(&buffer[..n]);
            chunk_bytes_read += n as u64;
            total_bytes_processed += n as u64;

            on_progress(ProgressEvent {
                operation: OperationKind::Verify,
                current_chunk: chunk_idx,
                total_chunks: manifest.total_chunks,
                bytes_processed: total_bytes_processed,
                total_bytes: manifest.original_file.size_bytes,
            });
        }

        // Verify streamed byte count against expected chunk size
        if chunk_bytes_read < chunk.size_bytes {
            return Err(ChunkerError::SizeMismatch {
                file: chunk_filename,
                expected: chunk.size_bytes,
                actual: chunk_bytes_read,
            });
        }

        // Drain to EOF to report the exact total stream length.
        // Once any surplus byte has been observed, a SizeMismatch error is guaranteed;
        // if cancellation is requested during subsequent drain reads, we stop draining
        // and immediately return SizeMismatch with the count observed so far, preserving
        // the size error rather than returning Cancelled.
        let mut extra_bytes = 0u64;
        loop {
            let extra_n = read_retrying_unstable(&mut reader, &mut buffer)?;
            if extra_n == 0 {
                break;
            }
            extra_bytes = extra_bytes
                .checked_add(extra_n as u64)
                .ok_or_else(|| ChunkerError::InvalidArgument("Chunk size overflow".into()))?;

            if is_cancelled(cancel_token) {
                // Surplus was already observed; size error takes precedence over cancellation.
                break;
            }
        }
        if extra_bytes > 0 {
            let actual = chunk
                .size_bytes
                .checked_add(extra_bytes)
                .ok_or_else(|| ChunkerError::InvalidArgument("Chunk size overflow".into()))?;
            return Err(ChunkerError::SizeMismatch {
                file: chunk_filename,
                expected: chunk.size_bytes,
                actual,
            });
        }

        // Verify chunk hash
        let actual_chunk_hash = chunk_hasher.finalize().to_hex().to_string();
        if actual_chunk_hash != chunk.blake3 {
            return Err(ChunkerError::HashMismatch {
                file: chunk_filename,
                expected: chunk.blake3.clone(),
                actual: actual_chunk_hash,
            });
        }
    }

    // Verify master hash
    let actual_master_hash = master_hasher.finalize().to_hex().to_string();
    if actual_master_hash != manifest.original_file.blake3 {
        return Err(ChunkerError::HashMismatch {
            file: manifest.original_file.name.clone(),
            expected: manifest.original_file.blake3.clone(),
            actual: actual_master_hash,
        });
    }

    // Final cancellation check before returning success
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    Ok(())
}
