use std::path::Path;

use crate::error::ChunkerError;
use crate::security::safe_parent;

/// Checks that the filesystem hosting `target_dir` has sufficient free space.
///
/// Required space is calculated as `(total_bytes - existing_valid_bytes) + overhead`,
/// using saturating arithmetic.
pub fn check_free_space(
    target_dir: &Path,
    total_bytes: u64,
    existing_valid_bytes: u64,
    overhead: u64,
) -> Result<(), ChunkerError> {
    let required = total_bytes
        .saturating_sub(existing_valid_bytes)
        .saturating_add(overhead);

    let dir = if target_dir.exists() {
        target_dir
    } else {
        safe_parent(target_dir)
    };

    let available = fs4::available_space(dir)?;
    if available < required {
        return Err(ChunkerError::InsufficientSpace {
            required,
            available,
            target: dir.to_path_buf(),
        });
    }
    Ok(())
}

fn handle_remove_result(res: std::io::Result<()>) -> Result<(), ChunkerError> {
    match res {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ChunkerError::Io(e)),
    }
}

/// Cleans up stale temporary files left behind by interrupted operations.
///
/// Strictly scoped to delete only `part-*.chk.tmp` and `manifest.json.tmp`.
pub fn clean_stale_tmp_files(bundle_dir: &Path) -> Result<(), ChunkerError> {
    if let Ok(entries) = std::fs::read_dir(bundle_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let is_stale_chunk = name.starts_with("part-") && name.ends_with(".chk.tmp");
                    let is_stale_manifest = name == "manifest.json.tmp";
                    if is_stale_chunk || is_stale_manifest {
                        handle_remove_result(std::fs::remove_file(&path))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Cleans up orphan chunk files with index >= `start_index`.
///
/// Strictly scoped to delete only files matching `part-XXXX.chk` where the parsed
/// integer index is greater than or equal to `start_index`.
pub fn clean_orphan_chunks(bundle_dir: &Path, start_index: usize) -> Result<(), ChunkerError> {
    if let Ok(entries) = std::fs::read_dir(bundle_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if let Some(rest) = name
                        .strip_prefix("part-")
                        .and_then(|s| s.strip_suffix(".chk"))
                    {
                        if let Ok(idx) = rest.parse::<usize>() {
                            if idx >= start_index {
                                handle_remove_result(std::fs::remove_file(&path))?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn test_handle_remove_result_ok() {
        assert!(handle_remove_result(Ok(())).is_ok());
    }

    #[test]
    fn test_handle_remove_result_not_found_is_ok() {
        let not_found = Error::new(ErrorKind::NotFound, "file not found");
        assert!(handle_remove_result(Err(not_found)).is_ok());
    }

    #[test]
    fn test_handle_remove_result_permission_denied_propagates() {
        let perm_denied = Error::new(ErrorKind::PermissionDenied, "permission denied");
        let res = handle_remove_result(Err(perm_denied));
        match res {
            Err(ChunkerError::Io(err)) => {
                assert_eq!(err.kind(), ErrorKind::PermissionDenied);
            }
            other => panic!("expected ChunkerError::Io(PermissionDenied), got {other:?}"),
        }
    }
}
