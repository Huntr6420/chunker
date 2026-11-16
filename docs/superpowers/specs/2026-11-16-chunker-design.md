# Design Document: Fast Streaming File Chunker & Dechunker

**Revision:** 2
**Date:** 2026-11-16  
**Status:** Approved / Ready for Implementation Planning  
**Spec File:** `docs/superpowers/specs/2026-11-16-chunker-design.md`  
**Author:** Antigravity (with User Collaboration)  

---

## 1. Intent and The "Why"

Users frequently need to transfer, store, or archive massive files (10 GB to 100+ GB) across media or cloud services that impose file size limits (FAT32, cloud upload limits, chunked backup systems).

This project implements a high-throughput, memory-bounded, cross-platform file chunker and dechunker in Rust. It is engineered from day one as a dual-crate workspace:
* `core`: A headless, deterministic library with zero terminal/UI dependencies, exposing synchronous streaming APIs with callback-driven progress reporting, RAII cleanup guards, and atomic cancellation hooks.
* `cli`: A user-facing CLI binary offering real-time `indicatif` progress bars, graceful signal handling (with a two-stage `Ctrl+C`), explicit exit codes (`0`, `1`, `2`, `130`), and friendly error reporting via `anyhow`.

The architecture ensures that a Tauri desktop application can directly wrap the `core` crate in the future without modifying any chunking or hashing code.

---

## 2. Goals, Non-Goals, and Anti-Goals

### Goals
* **Memory Footprint:** Use fixed-size 1 MiB I/O buffers; manifest metadata grows with chunk count. No constant total resident-memory ceiling is guaranteed.
* **Single-Pass Cryptographic Verification:** Simultaneously compute per-chunk and whole-file BLAKE3 hashes in a single streaming read pass during splitting and merging.
* **0-Byte File Support:** Correctly handles empty (0-byte) source files: `total_chunks = 0`, `chunks = []`, master BLAKE3 hash of empty input (`af1349b9...`), recreating a 0-byte file on merge.
* **Self-Contained Bundles:** Splitting creates a dedicated bundle directory (`<filename>.chunks/`) containing `manifest.json` and numbered chunks (`part-0000.chk`, `part-0001.chk`, ...).
* **Configurable Chunk Sizes:** Default 500 MiB; public split and CLI require at least 1 MiB. The final chunk may be smaller; manifest validation accepts any positive chunk size, and the test seam allows smaller sizes.
* **Interrupted I/O Resilience:** All read loops automatically retry on `std::io::ErrorKind::Interrupted` without dropping data or failing prematurely.
* **Robust Resumability with Checkpointed Master Hasher:**
  * At split startup, deletes any pre-existing `manifest.json` and stale temporary files (`part-*.chk.tmp` and `manifest.json.tmp`).
  * During resume pass, checks cancellation tokens and emits progress events.
  * Clones `master_hasher` and saves `stream_position` before each chunk candidate.
  * Streams chunk bytes from source file to feed `master_hasher` and verify against the existing `.chk` file.
  * If a chunk mismatches, is truncated, or is missing: restores `master_hasher` from checkpoint, seeks source file back, and resumes writing from that chunk without state corruption or double-hashing.
  * `total_bytes_read` initializes to the exact sum of verified chunk sizes (`recorded_chunks.iter().map(|c| c.size_bytes).sum()`).
* **Chunk Count Assertion:** Asserts `recorded_chunks.len() == total_chunks` prior to writing `manifest.json`.
* **Disk Space Pre-Allocation (Post-Resume):** Disk space check runs *after* the resume pass, accurately subtracting verified existing chunk sizes from required bytes.
* **Safe Scoped Cleanup:** Startup cleanup is strictly limited to `part-*.chk.tmp` and `manifest.json.tmp`. Resume cleanup deletes orphan `part-XXXX.chk` chunks with index `>= resume_chunk_index`.
* **Atomic Writes & Safe Temporary File Naming:**
  * Temporary files are always named by appending `.tmp` to the target filename (e.g., `part-0000.chk.tmp`, `archive.tar.gz.tmp`). `with_extension` is strictly avoided to prevent stripping double extensions.
  * Chunks are atomically renamed to `.chk` only once fully written, hashed, and flushed via `sync_all()`.
  * Merged files are written to `<output>.tmp`, flushed via `sync_all()`, and atomically renamed to the final destination upon successful hash validation.
* **Atomic Manifest Writing:** `manifest.json` is written to `manifest.json.tmp`, flushed via `sync_all()`, and atomically renamed to `manifest.json`.
* **Manifest Consistency Checks:** Require positive chunk size, matching counts, contiguous indexes, exact sizes, lowercase 64-character hashes and checked size sums. Empty-file master hash must equal `blake3::hash(b"")`; reject unknown JSON fields.
* **Security & Path Sanitization:**
  * Split extracts the final input component using both `/` and `\`. Manifest names must already be plain safe filenames; reject path separators and traversal instead of stripping them.
  * Safe parent path resolution: falls back to `.` for empty or missing parents.
  * Use the substring before the first dot to check case-insensitively against Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`).
  * Reject filenames containing invalid characters (`<`, `>`, `:`, `"`, `/`, `\`, `|`, `?`, `*`, control chars 0–31) or trailing dots/spaces.
* **Overwrite Protection:** Without `--force`, check for destination existence before writing and immediately before `fs::rename`. A small race remains between the second check and rename. With force, rely on `fs::rename` and propagate platform/filesystem errors; do not delete the old destination first.
* **RAII Cleanup Guard & Drop Order:** Active `.tmp` files are tracked by `TmpFileGuard` (providing `TmpFileGuard::empty()`). In split/merge loops, `chunk_guard` is declared *before* `chunk_writer`, ensuring Rust's LIFO drop order closes the file handle before attempting deletion on Windows.
* **Strict Stream Length Verification:** Returns `ChunkerError::Io` if total bytes read is less than expected (`UnexpectedEof`) or exceeds `total_file_size` (input file modified during split).
* **Dual Ctrl+C Signal Handling & Exit Code 130:**
  * First `Ctrl+C`: sets cancellation flag, triggering immediate graceful stop and RAII `.tmp` cleanup; CLI exits with code `130`.
  * Second `Ctrl+C`: immediately forces process exit (`std::process::exit(130)`).
* **Two-Tier Verification with Master Hash Validation:**
  * Standalone `chunker verify <bundle>`: Verifies all chunk sizes and hashes, then streams through all chunks to compute and verify the reconstructed master BLAKE3 hash against `manifest.original_file.blake3`.
  * Inline `chunker merge <bundle>`: Verifies chunk sizes upfront, checks chunk hashes on the fly, and validates the master BLAKE3 hash before finalizing.

### Non-Goals (Deferred to Future Versions)
* Per-chunk compression (e.g., `zstd`).
* Content-Defined Chunking (FastCDC).
* Multi-threaded parallel chunk I/O.
* Desktop UI (Tauri application in subsequent milestone).

---

## 3. Constraints & Crate Stack

* **Rust Edition:** 2024.
* **Toolchain:** Pin Rust 1.85.0 with rustfmt and clippy; commit `Cargo.lock`.
* **Dependencies:**
  * `core`:
    * `blake3 = "1.5"`
    * `serde = { version = "1.0", features = ["derive"] }`
    * `serde_json = "1.0"`
    * `thiserror = "2.0"`
    * `fs4 = "0.12"` (verified compiling on Windows)
    * Dev-dependency: `tempfile = "3"`.
  * `cli`:
    * `chunker-core = { path = "../core" }`; binary name `chunker`.
    * `clap = { version = "4.5", features = ["derive"] }`
    * `indicatif = "0.17"`
    * `anyhow = "1.0"`
    * `ctrlc = "3.4"`
    * Dev-dependency: `tempfile = "3"`.

All tests use `tempfile::tempdir()` for temporary directories and retain its guard.

---

## Public API

The package is `chunker-core`, imported as `chunker_core`. The hidden test interfaces below are non-stable and may change.

```rust
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
pub const BUFFER_SIZE: usize = 1024 * 1024;
pub const DEFAULT_CHUNK_SIZE: u64 = 500 * 1024 * 1024;
pub const MIN_CHUNK_SIZE: u64 = 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind { Split, Merge, Verify }
#[derive(Debug, Clone)]
pub struct ProgressEvent { pub operation: OperationKind, pub current_chunk: usize, pub total_chunks: usize, pub bytes_processed: u64, pub total_bytes: u64 }
#[derive(Debug, Clone)]
pub struct SplitSummary { pub bundle_path: PathBuf, pub reused_chunks: usize }
pub fn split_file<F>(input_path: &Path, output_dir: &Path, chunk_size: u64, cancel_token: Option<&Arc<AtomicBool>>, on_progress: F) -> Result<SplitSummary, ChunkerError> where F: FnMut(ProgressEvent);
pub fn verify_bundle<F>(bundle: &Path, cancel_token: Option<&Arc<AtomicBool>>, on_progress: F) -> Result<(), ChunkerError> where F: FnMut(ProgressEvent);
pub fn merge_bundle<F>(bundle: &Path, output: Option<&Path>, force: bool, cancel_token: Option<&Arc<AtomicBool>>, on_progress: F) -> Result<PathBuf, ChunkerError> where F: FnMut(ProgressEvent);
pub struct TmpFileGuard { /* private state */ }
impl TmpFileGuard { pub fn new(path: PathBuf) -> Self; pub fn empty() -> Self; pub fn reset(&mut self, path: PathBuf); pub fn disarm(&mut self); }
pub fn is_cancelled(token: Option<&Arc<AtomicBool>>) -> bool;
#[doc(hidden)] pub fn read_retrying_unstable<R: std::io::Read + ?Sized>(reader: &mut R, buffer: &mut [u8]) -> std::io::Result<usize>;
```

`SplitSummary.reused_chunks` counts validated existing chunks reused. The `#[doc(hidden)] pub` non-stable integration-test seam `split_with_reader_unstable` accepts a generic `Read + Seek`, expected size, chunk and buffer sizes, output, cancellation and progress context. The shared reader retries `Interrupted`. Invalid public split arguments produce `ChunkerError::InvalidArgument(String)`.

## 4. Detailed Component Design

### 4.1 Error Handling & Exit Codes

```rust
#[derive(Debug, thiserror::Error)]
pub enum ChunkerError {
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Manifest error: {0}")]
    Manifest(#[from] serde_json::Error),

    #[error("Hash mismatch for {file}: expected {expected}, got {actual}")]
    HashMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("Chunk size mismatch for {file}: expected {expected} bytes, got {actual} bytes")]
    SizeMismatch {
        file: String,
        expected: u64,
        actual: u64,
    },

    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("Destination already exists: {0}. Use --force to overwrite.")]
    DestinationAlreadyExists(std::path::PathBuf),

    #[error("Insufficient disk space on {target}: required {required} bytes, available {available} bytes")]
    InsufficientSpace {
        required: u64,
        available: u64,
        target: std::path::PathBuf,
    },

    #[error("Invalid or unsafe filename: {0}")]
    InvalidFilename(String),

    #[error("Operation cancelled by user")]
    Cancelled,
}
```

#### Exit Codes
* `0`: Success
* `1`: Operational error (I/O, hash mismatch, size mismatch, space error, destination exists)
* `2`: Argument / Usage error, including `InvalidArgument`
* `130`: Cancelled via SIGINT / `ChunkerError::Cancelled`

---

### 4.2 Manifest Schema & Strict Consistency Rules

#### Schema
```json
{
  "version": 1,
  "hash_algorithm": "blake3",
  "original_file": {
    "name": "archive.iso",
    "size_bytes": 10737418240,
    "blake3": "4b825dc642cb6eb9a060e54b3c579e8c4809a6f2202f5d1e2f5b6b1f5f8aca92"
  },
  "chunk_size_bytes": 524288000,
  "total_chunks": 21,
  "chunks": [
    {
      "index": 0,
      "file_name": "part-0000.chk",
      "size_bytes": 524288000,
      "blake3": "7a3f5c9e2b8104d4e3a890123456789abcdef0123456789abcdef0123456789a"
    }
  ]
}
```

#### Manifest Validation (Strict Lowercase Hex)

Use `#[serde(deny_unknown_fields)]` on each manifest structure. Validate a plain safe original filename; require positive `chunk_size_bytes` for all manifests and the exact empty BLAKE3 hash for a zero-byte file.
```rust
pub fn validate_hex_hash(hash: &str) -> Result<(), ChunkerError> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
        return Err(ChunkerError::InvalidManifest(format!(
            "Invalid BLAKE3 hash (must be 64-character lowercase hex): {hash}"
        )));
    }
    Ok(())
}

pub fn validate_manifest(m: &Manifest) -> Result<(), ChunkerError> {
    validate_plain_filename(&m.original_file.name)?;
    if m.version != 1 || m.hash_algorithm != "blake3" {
        return Err(ChunkerError::InvalidManifest("Unsupported version or hash algorithm".into()));
    }
    validate_hex_hash(&m.original_file.blake3)?;
    if m.total_chunks != m.chunks.len() {
        return Err(ChunkerError::InvalidManifest("total_chunks does not match chunks array length".into()));
    }

    if m.chunk_size_bytes == 0 { return Err(ChunkerError::InvalidManifest("chunk_size_bytes must be positive".into())); }
    if m.original_file.size_bytes == 0 {
        if m.total_chunks != 0 || !m.chunks.is_empty() {
            return Err(ChunkerError::InvalidManifest("0-byte file must have 0 chunks".into()));
        }
        if m.original_file.blake3 != blake3::hash(b"").to_hex().as_str() { return Err(ChunkerError::InvalidManifest("Incorrect empty-file hash".into())); }
        return Ok(());
    }

    let mut running_sum: u64 = 0;
    for (i, chunk) in m.chunks.iter().enumerate() {
        if chunk.index != i {
            return Err(ChunkerError::InvalidManifest(format!("Chunk index gap at {i}")));
        }
        validate_hex_hash(&chunk.blake3)?;
        let expected_name = format!("part-{:04}.chk", i);
        if chunk.file_name != expected_name {
            return Err(ChunkerError::InvalidManifest(format!(
                "Chunk filename mismatch: expected {expected_name}, got {}", chunk.file_name
            )));
        }

        if i < m.total_chunks - 1 {
            if chunk.size_bytes != m.chunk_size_bytes {
                return Err(ChunkerError::InvalidManifest(format!("Non-final chunk {i} does not match chunk_size_bytes")));
            }
        } else {
            if chunk.size_bytes == 0 || chunk.size_bytes > m.chunk_size_bytes {
                return Err(ChunkerError::InvalidManifest(format!("Invalid final chunk size: {}", chunk.size_bytes)));
            }
        }

        running_sum = running_sum.checked_add(chunk.size_bytes)
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
```

---

### 4.3 Safe Path Utilities & Sanitization

```rust
pub fn safe_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

pub fn make_tmp_path(path: &Path) -> PathBuf {
    let mut os_string = path.as_os_str().to_os_string();
    os_string.push(".tmp");
    PathBuf::from(os_string)
}

pub fn validate_plain_filename(file_name: &str) -> Result<String, ChunkerError> {
    if file_name.is_empty() || file_name == "." || file_name == ".." { return Err(ChunkerError::InvalidFilename("Filename is empty or invalid".into())); }

    const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    if file_name.chars().any(|c| INVALID_CHARS.contains(&c) || (c as u32) < 32) {
        return Err(ChunkerError::InvalidFilename(format!(
            "Filename '{file_name}' contains illegal or control characters"
        )));
    }

    if file_name.ends_with('.') || file_name.ends_with(' ') {
        return Err(ChunkerError::InvalidFilename(format!(
            "Filename '{file_name}' has illegal trailing space or dot"
        )));
    }

    let first_stem = file_name.split('.').next().unwrap_or(file_name).trim().to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    if RESERVED.contains(&first_stem.as_str()) {
        return Err(ChunkerError::InvalidFilename(format!(
            "Filename '{file_name}' prefix '{first_stem}' is a reserved Windows device name"
        )));
    }

    Ok(file_name.to_string())
}
```

---

### 4.4 Disk Space Pre-allocation Check (`check_free_space`)

```rust
pub fn check_free_space(
    target_dir: &Path,
    total_bytes: u64,
    existing_valid_bytes: u64,
    overhead: u64,
) -> Result<(), ChunkerError> {
    let required = total_bytes.saturating_sub(existing_valid_bytes).saturating_add(overhead);
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
```

---

### 4.5 Scoped Cleanup Functions

```rust
pub fn clean_stale_tmp_files(bundle_dir: &Path) -> Result<(), ChunkerError> {
    if let Ok(entries) = std::fs::read_dir(bundle_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let is_stale_chunk = name.starts_with("part-") && name.ends_with(".chk.tmp");
                    let is_stale_manifest = name == "manifest.json.tmp";
                    if is_stale_chunk || is_stale_manifest {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn clean_orphan_chunks(bundle_dir: &Path, start_index: usize) -> Result<(), ChunkerError> {
    if let Ok(entries) = std::fs::read_dir(bundle_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if let Some(rest) = name.strip_prefix("part-").and_then(|s| s.strip_suffix(".chk")) {
                        if let Ok(idx) = rest.parse::<usize>() {
                            if idx >= start_index {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
```

---

### 4.6 Split Engine (Interrupted Reads, Drop Order, and Chunk Count Assertion)

Code samples in this section are illustrative; the Public API and implementation plan take precedence if a sample differs. Use `div_ceil`, bound byte counts using `u64::min` before casting, and use `is_cancelled`. Progress indexes identify the chunk containing the last byte of a read, and empty files must not display a chunk number.

```rust
// 1. Delete old manifest.json and clean stale tmp files
let old_manifest = bundle_dir.join("manifest.json");
if old_manifest.exists() {
    let _ = std::fs::remove_file(old_manifest);
}
clean_stale_tmp_files(&bundle_dir)?;

// 2. Resume Fast-Forward with Interrupted Retry & Progress
let mut resume_chunk_index = 0;
let mut recorded_chunks: Vec<ChunkInfo> = Vec::new();
let mut resume_bytes_processed = 0u64;

while resume_chunk_index < total_chunks {
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    let chunk_filename = format!("part-{:04}.chk", resume_chunk_index);
    let chunk_path = bundle_dir.join(&chunk_filename);
    if !chunk_path.exists() {
        break;
    }

    let expected_size = if resume_chunk_index == total_chunks - 1 {
        last_chunk_size
    } else {
        chunk_size
    };

    let meta = chunk_path.metadata()?;
    if meta.len() != expected_size {
        break;
    }

    let checkpoint_hasher = master_hasher.clone();
    let checkpoint_pos = input_file.stream_position()?;

    let mut chunk_hasher = blake3::Hasher::new();
    let mut chunk_bytes_read = 0u64;
    let mut read_failed = false;

    while chunk_bytes_read < expected_size {
        if is_cancelled(cancel_token) {
            return Err(ChunkerError::Cancelled);
        }
        let to_read = (expected_size - chunk_bytes_read).min(buffer.len() as u64) as usize;
        let n = match input_file.read(&mut buffer[..to_read]) {
            Ok(0) => {
                read_failed = true;
                break;
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ChunkerError::Io(e)),
        };
        master_hasher.update(&buffer[..n]);
        chunk_hasher.update(&buffer[..n]);
        chunk_bytes_read += n as u64;
    }

    if read_failed || chunk_bytes_read != expected_size {
        master_hasher = checkpoint_hasher;
        input_file.seek(SeekFrom::Start(checkpoint_pos))?;
        break;
    }

    let existing_hash = hash_file(&chunk_path, &mut buffer)?;
    if chunk_hasher.finalize().to_hex().as_str() != existing_hash {
        master_hasher = checkpoint_hasher;
        input_file.seek(SeekFrom::Start(checkpoint_pos))?;
        break;
    }

    resume_bytes_processed += expected_size;
    on_progress(ProgressEvent {
        operation: OperationKind::Split,
        current_chunk: resume_chunk_index,
        total_chunks,
        bytes_processed: resume_bytes_processed,
        total_bytes: total_file_size,
    });

    recorded_chunks.push(ChunkInfo {
        index: resume_chunk_index,
        file_name: chunk_filename,
        size_bytes: expected_size,
        blake3: existing_hash,
    });
    resume_chunk_index += 1;
}

// 3. Clean orphan chunks from resume index forward
clean_orphan_chunks(&bundle_dir, resume_chunk_index)?;

// 4. Disk space check AFTER resume pass
let existing_valid_bytes: u64 = recorded_chunks.iter().map(|c| c.size_bytes).sum();
check_free_space(&bundle_dir, total_file_size, existing_valid_bytes, 1024 * 1024)?;

// 5. Initialize total_bytes_read to exact sum of verified chunks
let mut total_bytes_read: u64 = existing_valid_bytes;
let mut current_chunk_written = 0u64;
let mut current_chunk_index = resume_chunk_index;

// IMPORTANT DROP ORDER: guard declared FIRST (dropped SECOND), writer declared SECOND (dropped FIRST)
let mut chunk_guard = TmpFileGuard::empty();
let mut chunk_writer: Option<File> = None;
let mut chunk_hasher = blake3::Hasher::new();

loop {
    let bytes_read = match input_file.read(&mut buffer) {
        Ok(0) => break,
        Ok(n) => n,
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
        Err(e) => return Err(ChunkerError::Io(e)),
    };

    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    if total_bytes_read + (bytes_read as u64) > total_file_size {
        return Err(ChunkerError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Input file grew during split: expected {total_file_size} bytes, but read more"),
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
            last_chunk_size
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
            let mut file = chunk_writer.take().unwrap();
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

    total_bytes_read += bytes_read as u64;
    on_progress(ProgressEvent {
        operation: OperationKind::Split,
        current_chunk: last_chunk_index,
        total_chunks,
        bytes_processed: total_bytes_read,
        total_bytes: total_file_size,
    });
}

if total_bytes_read != total_file_size {
    return Err(ChunkerError::Io(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        format!("File ended prematurely: read {total_bytes_read} bytes, expected {total_file_size}"),
    )));
}

// 6. Assert chunk count before writing manifest
if recorded_chunks.len() != total_chunks {
    return Err(ChunkerError::InvalidManifest(format!(
        "Recorded chunk count ({}) does not match expected total ({})",
        recorded_chunks.len(),
        total_chunks
    )));
}

let master_hash = master_hasher.finalize().to_hex().to_string();
let manifest = Manifest {
    version: 1,
    hash_algorithm: "blake3".into(),
    original_file: OriginalFileInfo {
        name: safe_name,
        size_bytes: total_file_size,
        blake3: master_hash,
    },
    chunk_size_bytes: chunk_size,
    total_chunks,
    chunks: recorded_chunks,
};

save_manifest_atomic(&bundle_dir, &manifest)?;
Ok(bundle_dir)
```

---

### 4.7 Merge and Verify Algorithms

#### `merge_bundle`
1. Load & validate manifest.
2. Resolve destination path (`safe_parent`, sanitize original filename).
3. Check destination existence: if exists and `!force`, return `Err(DestinationAlreadyExists)`.
4. Check free space on destination parent directory.
5. For a 0-byte file validate its empty master hash, then create a guarded empty temporary file and use the same finalization path.
6. Open destination `.tmp` file guarded by `TmpFileGuard`.
7. Initialize master hasher.
8. For each chunk:
   * Verify size matches `chunk.size_bytes` via metadata.
   * Stream chunk in 1 MiB buffers, retrying on `ErrorKind::Interrupted`.
   * Update chunk hasher and master hasher.
   * Verify finalized chunk hash matches `chunk.blake3`.
9. Verify finalized master hash matches `manifest.original_file.blake3`.
10. Flush via `sync_all`, close, re-check destination without force, then use `fs::rename` and disarm the guard. The check-to-rename race remains; replacement depends on platform and filesystem.

#### `verify_bundle`
1. Load & validate manifest.
2. If 0-byte file, return `Ok(())` only after validating the empty master hash.
3. Initialize master hasher.
4. For each chunk:
   * Upfront size check against `chunk.size_bytes`.
   * Stream chunk in 1 MiB buffers, retrying on `ErrorKind::Interrupted`.
   * Update chunk hasher and master hasher.
   * Verify finalized chunk hash matches `chunk.blake3`.
5. Verify finalized master hash matches `manifest.original_file.blake3`.

---

### 4.8 CLI Signal Handler (Dual Ctrl+C)

```rust
static SIGNAL_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn setup_signal_handler(cancel_token: Arc<AtomicBool>) -> anyhow::Result<()> {
    ctrlc::set_handler(move || {
        let count = SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            cancel_token.store(true, Ordering::SeqCst);
            eprintln!("\n[!] Cancellation requested. Cleaning up active temporary files... (Press Ctrl+C again to force exit)");
        } else {
            eprintln!("\n[!] Force exiting immediately.");
            std::process::exit(130);
        }
    })?;
    Ok(())
}
```
