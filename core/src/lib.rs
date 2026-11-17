pub const BUFFER_SIZE: usize = 1024 * 1024; // 1 MiB
pub const DEFAULT_CHUNK_SIZE: u64 = 500 * 1024 * 1024; // 500 MiB
pub const MIN_CHUNK_SIZE: u64 = 1024 * 1024; // 1 MiB

pub mod error;
pub mod fs_utils;
pub mod guard;
pub mod manifest;
pub mod progress;
pub mod security;

pub use error::ChunkerError;
pub use fs_utils::{check_free_space, clean_orphan_chunks, clean_stale_tmp_files};
pub use guard::TmpFileGuard;
pub use manifest::{
    ChunkInfo, Manifest, OriginalFileInfo, load_manifest, resolve_bundle_and_manifest,
    save_manifest_atomic, validate_hex_hash, validate_manifest,
};
pub use progress::{OperationKind, ProgressEvent};
pub use security::{extract_filename, make_tmp_path, safe_parent, validate_plain_filename};
