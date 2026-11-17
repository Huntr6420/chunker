use std::path::{Path, PathBuf};

use crate::error::ChunkerError;

/// Returns the parent directory of `path`, or `"."` if the parent is empty or missing.
pub fn safe_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Constructs a temporary file path by appending `.tmp` to the target path.
///
/// Avoids `with_extension("tmp")` to prevent stripping existing extensions
/// (e.g. `archive.tar.gz` becomes `archive.tar.gz.tmp`).
pub fn make_tmp_path(path: &Path) -> PathBuf {
    let mut os_string = path.as_os_str().to_os_string();
    os_string.push(".tmp");
    PathBuf::from(os_string)
}

/// Validates that a string is a safe, plain filename.
///
/// Rejects path separators, traversal (`.` or `..`), control characters,
/// characters forbidden across major OSes (`< > : " / \ | ? *`), trailing
/// spaces or dots, and Windows reserved device names (checked before first dot).
pub fn validate_plain_filename(file_name: &str) -> Result<String, ChunkerError> {
    if file_name.is_empty() || file_name == "." || file_name == ".." {
        return Err(ChunkerError::InvalidFilename(
            "Filename is empty or invalid".into(),
        ));
    }

    const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    if file_name
        .chars()
        .any(|c| INVALID_CHARS.contains(&c) || (c as u32) < 32)
    {
        return Err(ChunkerError::InvalidFilename(format!(
            "Filename '{file_name}' contains illegal or control characters"
        )));
    }

    if file_name.ends_with('.') || file_name.ends_with(' ') {
        return Err(ChunkerError::InvalidFilename(format!(
            "Filename '{file_name}' has illegal trailing space or dot"
        )));
    }

    let first_stem = file_name
        .split('.')
        .next()
        .unwrap_or(file_name)
        .trim()
        .to_ascii_uppercase();

    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    if RESERVED.contains(&first_stem.as_str()) {
        return Err(ChunkerError::InvalidFilename(format!(
            "Filename '{file_name}' prefix '{first_stem}' is a reserved Windows device name"
        )));
    }

    Ok(file_name.to_string())
}

/// Extracts the final component of an input path using both `/` and `\`,
/// and verifies that it is a safe plain filename.
pub fn extract_filename(input_path: &Path) -> Result<String, ChunkerError> {
    let s = input_path
        .to_str()
        .ok_or_else(|| ChunkerError::InvalidFilename("Path is not valid UTF-8".into()))?;

    let last = s
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .last()
        .ok_or_else(|| {
            ChunkerError::InvalidFilename("Path does not contain a valid filename".into())
        })?;

    validate_plain_filename(last)
}
