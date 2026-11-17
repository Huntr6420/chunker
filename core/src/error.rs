use std::path::PathBuf;

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
    DestinationAlreadyExists(PathBuf),

    #[error(
        "Insufficient disk space on {target}: required {required} bytes, available {available} bytes"
    )]
    InsufficientSpace {
        required: u64,
        available: u64,
        target: PathBuf,
    },

    #[error("Invalid or unsafe filename: {0}")]
    InvalidFilename(String),

    #[error("Operation cancelled by user")]
    Cancelled,
}
