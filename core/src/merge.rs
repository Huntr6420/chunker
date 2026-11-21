use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::BUFFER_SIZE;
use crate::error::ChunkerError;
use crate::fs_utils::check_free_space;
use crate::guard::TmpFileGuard;
use crate::manifest::{Manifest, load_manifest};
use crate::progress::{OperationKind, ProgressEvent};
use crate::security::{make_tmp_path, safe_parent};
use crate::stream::{is_cancelled, read_retrying_unstable};

/// Merges chunk files from a bundle directory back into the original file.
///
/// If `output` is `None`, the original filename is placed beside the bundle directory.
/// If `output` is an existing directory, the original filename is placed inside it.
/// Otherwise, `output` is used directly as the destination file path.
pub fn merge_bundle<F>(
    bundle: &Path,
    output: Option<&Path>,
    force: bool,
    cancel_token: Option<&Arc<AtomicBool>>,
    on_progress: F,
) -> Result<PathBuf, ChunkerError>
where
    F: FnMut(ProgressEvent),
{
    // 1. Cancellation check before filesystem I/O
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    // Inspect bundle path with metadata
    match std::fs::metadata(bundle) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(ChunkerError::InvalidArgument(format!(
                    "Bundle path must be a directory: {}",
                    bundle.display()
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Let NotFound fall through to load_manifest
        }
        Err(e) => return Err(ChunkerError::Io(e)),
    }

    // Load and validate manifest
    let manifest = load_manifest(bundle)?;

    // 2. Resolve destination path
    let filename = &manifest.original_file.name;
    let destination = match output {
        None => safe_parent(bundle).join(filename),
        Some(out) => {
            if out.is_dir() {
                out.join(filename)
            } else {
                out.to_path_buf()
            }
        }
    };

    // 3. Check output parent directory before space check or opening temporary file
    let parent = safe_parent(&destination);
    match std::fs::metadata(parent) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(ChunkerError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "Output parent directory does not exist: {}",
                        parent.display()
                    ),
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ChunkerError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Output parent directory does not exist: {}",
                    parent.display()
                ),
            )));
        }
        Err(e) => return Err(ChunkerError::Io(e)),
    }

    // Early destination existence check
    if destination.exists() && !force {
        return Err(ChunkerError::DestinationAlreadyExists(destination));
    }

    // 4. Free space check on parent directory
    check_free_space(parent, manifest.original_file.size_bytes, 0, 0)?;

    // Open temporary file guarded by TmpFileGuard
    let tmp_path = make_tmp_path(&destination);
    let mut guard = TmpFileGuard::empty();

    {
        let mut tmp_file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => {
                guard.reset(tmp_path.clone());
                file
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ChunkerError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "Temporary file already exists: {}. If it is a leftover from an earlier run, remove it and retry.",
                        tmp_path.display()
                    ),
                )));
            }
            Err(e) => return Err(ChunkerError::Io(e)),
        };

        let mut open_chunk = |idx: usize| -> std::io::Result<(File, u64)> {
            let chunk_filename = format!("part-{:04}.chk", idx);
            let chunk_path = bundle.join(chunk_filename);
            let file = File::open(&chunk_path)?;
            let meta = file.metadata()?;
            Ok((file, meta.len()))
        };

        merge_with_opener_unstable(
            &manifest,
            &mut open_chunk,
            &mut tmp_file,
            cancel_token,
            on_progress,
        )?;

        tmp_file.sync_all()?;
    }

    // 5. Without force, re-check whether destination exists before rename
    if !force && destination.exists() {
        return Err(ChunkerError::DestinationAlreadyExists(destination));
    }

    // Cancellation check immediately before rename
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    // Atomically rename temporary file to destination
    std::fs::rename(&tmp_path, &destination)?;

    // Disarm guard after successful rename
    guard.disarm();
    Ok(destination)
}

/// Non-stable integration-test seam for merge streaming and verification.
///
/// Streams chunks through an opener into the supplied writer, verifying chunk sizes,
/// surplus bytes, per-chunk BLAKE3 hashes, and the overall master BLAKE3 hash.
#[doc(hidden)]
pub fn merge_with_opener_unstable<R, O, W, F>(
    manifest: &Manifest,
    mut open_chunk: O,
    writer: &mut W,
    cancel_token: Option<&Arc<AtomicBool>>,
    mut on_progress: F,
) -> Result<(), ChunkerError>
where
    R: std::io::Read,
    O: FnMut(usize) -> std::io::Result<(R, u64)>,
    W: std::io::Write,
    F: FnMut(ProgressEvent),
{
    // Check cancellation at seam entry
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut master_hasher = blake3::Hasher::new();
    let mut total_bytes_processed = 0u64;

    for (chunk_idx, chunk) in manifest.chunks.iter().enumerate() {
        let chunk_filename = format!("part-{:04}.chk", chunk_idx);
        let (mut reader, meta_len) = open_chunk(chunk_idx)?;

        // Upfront metadata size comparison
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
            writer.write_all(&buffer[..n])?;

            chunk_bytes_read += n as u64;
            total_bytes_processed += n as u64;

            on_progress(ProgressEvent {
                operation: OperationKind::Merge,
                current_chunk: chunk_idx,
                total_chunks: manifest.total_chunks,
                bytes_processed: total_bytes_processed,
                total_bytes: manifest.original_file.size_bytes,
            });
        }

        // Early EOF check
        if chunk_bytes_read < chunk.size_bytes {
            return Err(ChunkerError::SizeMismatch {
                file: chunk_filename,
                expected: chunk.size_bytes,
                actual: chunk_bytes_read,
            });
        }

        // Surplus probe using the full buffer and retry helper
        let probe_n = read_retrying_unstable(&mut reader, &mut buffer)?;
        if probe_n > 0 {
            let actual = chunk
                .size_bytes
                .checked_add(probe_n as u64)
                .ok_or_else(|| ChunkerError::InvalidArgument("Chunk size overflow".into()))?;
            return Err(ChunkerError::SizeMismatch {
                file: chunk_filename,
                expected: chunk.size_bytes,
                actual,
            });
        }

        // Verify chunk BLAKE3 hash
        let actual_chunk_hash = chunk_hasher.finalize().to_hex().to_string();
        if actual_chunk_hash != chunk.blake3 {
            return Err(ChunkerError::HashMismatch {
                file: chunk_filename,
                expected: chunk.blake3.clone(),
                actual: actual_chunk_hash,
            });
        }
    }

    // Verify overall master BLAKE3 hash
    let actual_master_hash = master_hasher.finalize().to_hex().to_string();
    if actual_master_hash != manifest.original_file.blake3 {
        return Err(ChunkerError::HashMismatch {
            file: manifest.original_file.name.clone(),
            expected: manifest.original_file.blake3.clone(),
            actual: actual_master_hash,
        });
    }

    Ok(())
}
