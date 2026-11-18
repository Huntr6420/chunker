use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::error::ChunkerError;
use crate::fs_utils::{check_free_space, clean_orphan_chunks, clean_stale_tmp_files};
use crate::guard::TmpFileGuard;
use crate::manifest::{ChunkInfo, Manifest, OriginalFileInfo, save_manifest_atomic};
use crate::progress::{OperationKind, ProgressEvent};
use crate::security::{extract_filename, make_tmp_path, validate_plain_filename};
use crate::stream::{is_cancelled, read_retrying_unstable};
use crate::{BUFFER_SIZE, MIN_CHUNK_SIZE};

/// Summary of a file split operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitSummary {
    pub bundle_path: PathBuf,
    pub reused_chunks: usize,
}

/// Splits an input file into fixed-size chunks and creates a verified manifest in `<output_dir>/<filename>.chunks/`.
pub fn split_file<F>(
    input_path: &Path,
    output_dir: &Path,
    chunk_size: u64,
    cancel_token: Option<&Arc<AtomicBool>>,
    on_progress: F,
) -> Result<SplitSummary, ChunkerError>
where
    F: FnMut(ProgressEvent),
{
    if chunk_size < MIN_CHUNK_SIZE {
        return Err(ChunkerError::InvalidArgument(format!(
            "chunk size {chunk_size} is below MIN_CHUNK_SIZE ({MIN_CHUNK_SIZE})"
        )));
    }

    let original_filename = extract_filename(input_path)?;
    let mut file = File::open(input_path)?;
    let expected_size = file.metadata()?.len();

    split_with_reader_unstable(
        &mut file,
        &original_filename,
        expected_size,
        chunk_size,
        BUFFER_SIZE,
        output_dir,
        cancel_token,
        on_progress,
    )
}

/// Non-stable integration-test seam for file splitting.
///
/// Accepts a generic `Read + Seek` reader, original filename, expected size, chunk and buffer sizes,
/// output directory, cancellation token, and progress callback.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn split_with_reader_unstable<R: Read + Seek, F>(
    reader: &mut R,
    original_filename: &str,
    expected_size: u64,
    chunk_size: u64,
    buffer_size: usize,
    output_dir: &Path,
    cancel_token: Option<&Arc<AtomicBool>>,
    mut on_progress: F,
) -> Result<SplitSummary, ChunkerError>
where
    F: FnMut(ProgressEvent),
{
    validate_plain_filename(original_filename)?;

    if chunk_size == 0 {
        return Err(ChunkerError::InvalidArgument(
            "chunk_size must be positive".into(),
        ));
    }
    if buffer_size == 0 {
        return Err(ChunkerError::InvalidArgument(
            "buffer_size must be positive".into(),
        ));
    }

    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    let bundle_dir = output_dir.join(format!("{}.chunks", original_filename));
    std::fs::create_dir_all(&bundle_dir)?;

    let old_manifest = bundle_dir.join("manifest.json");
    match std::fs::remove_file(&old_manifest) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ChunkerError::Io(e)),
    }
    clean_stale_tmp_files(&bundle_dir)?;

    check_free_space(&bundle_dir, expected_size, 0, 1024 * 1024)?;

    if expected_size == 0 {
        // Probe for growth: a zero-expected source must have no bytes.
        let mut probe = [0u8; 1];
        match read_retrying_unstable(reader, &mut probe) {
            Ok(0) => {}
            Ok(_) => {
                return Err(ChunkerError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Input file grew during split: expected {expected_size} bytes, but read more"
                    ),
                )));
            }
            Err(e) => return Err(ChunkerError::Io(e)),
        }

        if is_cancelled(cancel_token) {
            return Err(ChunkerError::Cancelled);
        }

        clean_orphan_chunks(&bundle_dir, 0)?;

        let manifest = Manifest {
            version: 1,
            hash_algorithm: "blake3".into(),
            original_file: OriginalFileInfo {
                name: original_filename.to_string(),
                size_bytes: 0,
                blake3: blake3::hash(b"").to_hex().to_string(),
            },
            chunk_size_bytes: chunk_size,
            total_chunks: 0,
            chunks: Vec::new(),
        };

        if is_cancelled(cancel_token) {
            return Err(ChunkerError::Cancelled);
        }

        save_manifest_atomic(&bundle_dir, &manifest)?;

        return Ok(SplitSummary {
            bundle_path: bundle_dir,
            reused_chunks: 0,
        });
    }

    let total_chunks_u64 = expected_size.div_ceil(chunk_size);
    let total_chunks = usize::try_from(total_chunks_u64)
        .map_err(|_| ChunkerError::InvalidArgument("Total chunks exceeds usize capacity".into()))?;

    let mut buffer = vec![0u8; buffer_size];
    let mut total_bytes_read = 0u64;
    let mut current_chunk_index = 0usize;
    let mut current_chunk_written = 0u64;
    let mut recorded_chunks: Vec<ChunkInfo> = Vec::with_capacity(total_chunks);
    let mut master_hasher = blake3::Hasher::new();
    let mut chunk_hasher = blake3::Hasher::new();

    // Guard declared FIRST, writer declared SECOND for correct LIFO drop order on Windows.
    let mut chunk_guard = TmpFileGuard::empty();
    let mut chunk_writer: Option<File> = None;

    while total_bytes_read < expected_size {
        if is_cancelled(cancel_token) {
            return Err(ChunkerError::Cancelled);
        }

        let bytes_read = match read_retrying_unstable(reader, &mut buffer[..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(ChunkerError::Io(e)),
        };

        let new_total = total_bytes_read
            .checked_add(bytes_read as u64)
            .ok_or_else(|| {
                ChunkerError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Byte counter overflow during split",
                ))
            })?;

        if new_total > expected_size {
            return Err(ChunkerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Input file grew during split: expected {expected_size} bytes, but read more"
                ),
            )));
        }

        let mut buffer_offset = 0;
        let mut last_chunk_index = current_chunk_index;

        while buffer_offset < bytes_read {
            if chunk_writer.is_none() {
                let chunk_filename = format!("part-{:04}.chk", current_chunk_index);
                let final_chunk_path = bundle_dir.join(&chunk_filename);
                let tmp_chunk_path = make_tmp_path(&final_chunk_path);

                chunk_guard.reset(tmp_chunk_path.clone());
                chunk_writer = Some(File::create(&tmp_chunk_path)?);
                chunk_hasher.reset();
                current_chunk_written = 0;
            }

            let expected_this_chunk = if current_chunk_index == total_chunks - 1 {
                let rem = expected_size % chunk_size;
                if rem == 0 { chunk_size } else { rem }
            } else {
                chunk_size
            };

            let bytes_left = expected_this_chunk - current_chunk_written;
            let slice_len = ((bytes_read - buffer_offset) as u64).min(bytes_left) as usize;
            let slice = &buffer[buffer_offset..buffer_offset + slice_len];

            last_chunk_index = current_chunk_index;
            master_hasher.update(slice);
            chunk_writer.as_mut().unwrap().write_all(slice)?;
            chunk_hasher.update(slice);

            current_chunk_written += slice_len as u64;
            buffer_offset += slice_len;

            if current_chunk_written == expected_this_chunk {
                let file = chunk_writer.take().unwrap();
                file.sync_all()?;
                drop(file);

                let chunk_hash = chunk_hasher.finalize().to_hex().to_string();
                let chunk_filename = format!("part-{:04}.chk", current_chunk_index);
                let final_chunk_path = bundle_dir.join(&chunk_filename);
                let tmp_chunk_path = make_tmp_path(&final_chunk_path);

                std::fs::rename(&tmp_chunk_path, &final_chunk_path)?;
                chunk_guard.disarm();

                recorded_chunks.push(ChunkInfo {
                    index: current_chunk_index,
                    file_name: chunk_filename,
                    size_bytes: current_chunk_written,
                    blake3: chunk_hash,
                });

                current_chunk_index += 1;
            }
        }

        total_bytes_read = new_total;
        on_progress(ProgressEvent {
            operation: OperationKind::Split,
            current_chunk: last_chunk_index,
            total_chunks,
            bytes_processed: total_bytes_read,
            total_bytes: expected_size,
        });
    }

    if total_bytes_read < expected_size {
        return Err(ChunkerError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "File ended prematurely: read {total_bytes_read} bytes, expected {expected_size}"
            ),
        )));
    }

    // Probe for buffer-aligned growth: when the read loop exits because
    // total_bytes_read == expected_size, there may be unread bytes that the
    // `> expected_size` check inside the loop never saw.
    {
        let mut probe = [0u8; 1];
        match read_retrying_unstable(reader, &mut probe) {
            Ok(0) => {}
            Ok(_) => {
                return Err(ChunkerError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Input file grew during split: expected {expected_size} bytes, but read more"
                    ),
                )));
            }
            Err(e) => return Err(ChunkerError::Io(e)),
        }
    }

    if recorded_chunks.len() != total_chunks {
        return Err(ChunkerError::InvalidManifest(format!(
            "Recorded chunk count ({}) does not match expected total ({})",
            recorded_chunks.len(),
            total_chunks
        )));
    }

    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    clean_orphan_chunks(&bundle_dir, total_chunks)?;

    let master_hash = master_hasher.finalize().to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: original_filename.to_string(),
            size_bytes: expected_size,
            blake3: master_hash,
        },
        chunk_size_bytes: chunk_size,
        total_chunks,
        chunks: recorded_chunks,
    };

    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    save_manifest_atomic(&bundle_dir, &manifest)?;

    Ok(SplitSummary {
        bundle_path: bundle_dir,
        reused_chunks: 0,
    })
}
