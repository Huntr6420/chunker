use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ChunkerError;
use crate::guard::TmpFileGuard;
use crate::security::{make_tmp_path, safe_parent, validate_plain_filename};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OriginalFileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChunkInfo {
    pub index: usize,
    pub file_name: String,
    pub size_bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub hash_algorithm: String,
    pub original_file: OriginalFileInfo,
    pub chunk_size_bytes: u64,
    pub total_chunks: usize,
    pub chunks: Vec<ChunkInfo>,
}

/// Validates that `hash` is a 64-character lowercase hex BLAKE3 string.
pub fn validate_hex_hash(hash: &str) -> Result<(), ChunkerError> {
    if hash.len() != 64
        || !hash
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(ChunkerError::InvalidManifest(format!(
            "Invalid BLAKE3 hash (must be 64-character lowercase hex): {hash}"
        )));
    }
    Ok(())
}

/// Validates that a `Manifest` satisfies all structural and cryptographic consistency rules.
pub fn validate_manifest(m: &Manifest) -> Result<(), ChunkerError> {
    validate_plain_filename(&m.original_file.name)?;
    if m.version != 1 || m.hash_algorithm != "blake3" {
        return Err(ChunkerError::InvalidManifest(
            "Unsupported version or hash algorithm".into(),
        ));
    }
    validate_hex_hash(&m.original_file.blake3)?;
    if m.total_chunks != m.chunks.len() {
        return Err(ChunkerError::InvalidManifest(
            "total_chunks does not match chunks array length".into(),
        ));
    }

    if m.chunk_size_bytes == 0 {
        return Err(ChunkerError::InvalidManifest(
            "chunk_size_bytes must be positive".into(),
        ));
    }
    if m.original_file.size_bytes == 0 {
        if m.total_chunks != 0 || !m.chunks.is_empty() {
            return Err(ChunkerError::InvalidManifest(
                "0-byte file must have 0 chunks".into(),
            ));
        }
        if m.original_file.blake3 != blake3::hash(b"").to_hex().as_str() {
            return Err(ChunkerError::InvalidManifest(
                "Incorrect empty-file hash".into(),
            ));
        }
        return Ok(());
    }

    let mut running_sum: u64 = 0;
    for (i, chunk) in m.chunks.iter().enumerate() {
        if chunk.index != i {
            return Err(ChunkerError::InvalidManifest(format!(
                "Chunk index gap at {i}"
            )));
        }
        validate_hex_hash(&chunk.blake3)?;
        let expected_name = format!("part-{:04}.chk", i);
        if chunk.file_name != expected_name {
            return Err(ChunkerError::InvalidManifest(format!(
                "Chunk filename mismatch: expected {expected_name}, got {}",
                chunk.file_name
            )));
        }

        if i < m.total_chunks - 1 {
            if chunk.size_bytes != m.chunk_size_bytes {
                return Err(ChunkerError::InvalidManifest(format!(
                    "Non-final chunk {i} does not match chunk_size_bytes"
                )));
            }
        } else if chunk.size_bytes == 0 || chunk.size_bytes > m.chunk_size_bytes {
            return Err(ChunkerError::InvalidManifest(format!(
                "Invalid final chunk size: {}",
                chunk.size_bytes
            )));
        }

        running_sum = running_sum
            .checked_add(chunk.size_bytes)
            .ok_or_else(|| ChunkerError::InvalidManifest("Chunk size sum overflowed u64".into()))?;
    }

    if running_sum != m.original_file.size_bytes {
        return Err(ChunkerError::InvalidManifest(format!(
            "Sum of chunk sizes ({running_sum}) does not match original_file.size_bytes ({})",
            m.original_file.size_bytes
        )));
    }

    Ok(())
}

fn map_serde_error(e: serde_json::Error) -> ChunkerError {
    if e.is_io() {
        ChunkerError::Io(e.into())
    } else {
        ChunkerError::Manifest(e)
    }
}

fn parse_manifest<R: std::io::Read>(reader: R) -> Result<Manifest, ChunkerError> {
    serde_json::from_reader(reader).map_err(map_serde_error)
}

/// Resolves a path that may point either to a bundle directory or directly to `manifest.json`.
/// Returns `(bundle_dir, manifest_path)`.
pub fn resolve_bundle_and_manifest(path: &Path) -> Result<(PathBuf, PathBuf), ChunkerError> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };

    if path.is_dir() {
        let manifest_path = path.join("manifest.json");
        let bundle_dir = path.to_path_buf();
        Ok((bundle_dir, manifest_path))
    } else if path.is_file() || path.file_name().and_then(|n| n.to_str()) == Some("manifest.json") {
        // Any existing file path (or non-directory path named "manifest.json") is treated as a manifest.
        // Note: treating any arbitrary existing file as a manifest is known behavior kept for a later spec decision.
        let manifest_path = path.to_path_buf();
        let bundle_dir = safe_parent(path).to_path_buf();
        Ok((bundle_dir, manifest_path))
    } else {
        let manifest_path = path.join("manifest.json");
        let bundle_dir = path.to_path_buf();
        Ok((bundle_dir, manifest_path))
    }
}

/// Loads and validates a manifest from either a bundle directory or a direct manifest path.
pub fn load_manifest(path: &Path) -> Result<Manifest, ChunkerError> {
    let (_, manifest_path) = resolve_bundle_and_manifest(path)?;
    let file = std::fs::File::open(&manifest_path)?;
    let reader = std::io::BufReader::new(file);
    let manifest = parse_manifest(reader)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Atomically writes a validated manifest into `bundle_dir`.
///
/// Writes to `manifest.json.tmp` guarded by `TmpFileGuard`, flushes via `sync_all()`,
/// closes the file handle, renames to `manifest.json`, and disarms the guard.
pub fn save_manifest_atomic(
    bundle_dir: &Path,
    manifest: &Manifest,
) -> Result<PathBuf, ChunkerError> {
    validate_manifest(manifest)?;

    let manifest_path = bundle_dir.join("manifest.json");
    let tmp_path = make_tmp_path(&manifest_path);

    // Guard declared FIRST
    let mut guard = TmpFileGuard::empty();

    // Write through BufWriter, sync underlying file, and drop handle before rename
    {
        let file = match std::fs::OpenOptions::new()
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
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, manifest).map_err(map_serde_error)?;
        writer.flush()?;
        let file = writer.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
    }

    std::fs::rename(&tmp_path, &manifest_path)?;
    guard.disarm();

    Ok(manifest_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingReader {
        initial: &'static [u8],
        delivered: bool,
    }

    impl std::io::Read for FailingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.delivered {
                let n = self.initial.len().min(buf.len());
                buf[..n].copy_from_slice(&self.initial[..n]);
                self.delivered = true;
                Ok(n)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "simulated read error",
                ))
            }
        }
    }

    #[test]
    fn test_parse_manifest_propagates_io_error() {
        let reader = FailingReader {
            initial: b"{\"version\": 1, ",
            delivered: false,
        };
        let res = parse_manifest(reader);
        assert!(
            matches!(res, Err(ChunkerError::Io(_))),
            "Expected ChunkerError::Io, got {:?}",
            res
        );
    }
}
