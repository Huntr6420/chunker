use std::path::{Path, PathBuf};

/// RAII cleanup guard for active temporary files.
///
/// Automatically unlinks the protected temporary file when dropped,
/// unless explicitly disarmed via `disarm()`.
pub struct TmpFileGuard {
    path: Option<PathBuf>,
}

impl TmpFileGuard {
    /// Creates a new guard armed to protect and clean up the specified temporary file.
    pub fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Creates an unarmed, empty guard.
    pub fn empty() -> Self {
        Self { path: None }
    }

    /// Replaces the protected file path with a new path.
    ///
    /// If a file was previously protected, it is immediately deleted (unlinked)
    /// before tracking the new path.
    pub fn reset(&mut self, path: PathBuf) {
        if let Some(old_path) = self.path.take() {
            let _ = std::fs::remove_file(old_path);
        }
        self.path = Some(path);
    }

    /// Disarms the guard, preventing deletion of the temporary file upon drop.
    pub fn disarm(&mut self) {
        self.path = None;
    }

    /// Returns a reference to the currently protected path, if armed.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
