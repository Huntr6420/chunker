use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

use chunker_core::error::ChunkerError;
use chunker_core::fs_utils::{check_free_space, clean_orphan_chunks, clean_stale_tmp_files};
use chunker_core::guard::TmpFileGuard;
use chunker_core::progress::{OperationKind, ProgressEvent};
use chunker_core::security::{
    extract_filename, make_tmp_path, safe_parent, validate_plain_filename,
};

#[test]
fn test_chunker_error_formatting() {
    let err_arg = ChunkerError::InvalidArgument("chunk size too small".into());
    assert_eq!(
        err_arg.to_string(),
        "Invalid argument: chunk size too small"
    );

    let io_err = ChunkerError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "file not found",
    ));
    assert!(io_err.to_string().contains("file not found"));

    let json_err: Result<serde_json::Value, _> = serde_json::from_str("{ invalid");
    let manifest_err = ChunkerError::Manifest(json_err.unwrap_err());
    assert!(manifest_err.to_string().starts_with("Manifest error:"));

    let hash_err = ChunkerError::HashMismatch {
        file: "part-0000.chk".into(),
        expected: "aaa".into(),
        actual: "bbb".into(),
    };
    assert_eq!(
        hash_err.to_string(),
        "Hash mismatch for part-0000.chk: expected aaa, got bbb"
    );

    let size_err = ChunkerError::SizeMismatch {
        file: "part-0000.chk".into(),
        expected: 100,
        actual: 80,
    };
    assert_eq!(
        size_err.to_string(),
        "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 80 bytes"
    );

    let inv_manifest = ChunkerError::InvalidManifest("missing field".into());
    assert_eq!(inv_manifest.to_string(), "Invalid manifest: missing field");

    let dest_err = ChunkerError::DestinationAlreadyExists(PathBuf::from("output.iso"));
    assert_eq!(
        dest_err.to_string(),
        "Destination already exists: output.iso. Use --force to overwrite."
    );

    let space_err = ChunkerError::InsufficientSpace {
        required: 1000,
        available: 500,
        target: PathBuf::from("D:\\chunks"),
    };
    assert_eq!(
        space_err.to_string(),
        "Insufficient disk space on D:\\chunks: required 1000 bytes, available 500 bytes"
    );

    let filename_err = ChunkerError::InvalidFilename("bad:name".into());
    assert_eq!(
        filename_err.to_string(),
        "Invalid or unsafe filename: bad:name"
    );

    let cancel_err = ChunkerError::Cancelled;
    assert_eq!(cancel_err.to_string(), "Operation cancelled by user");
}

#[test]
fn test_progress_event_and_operation_kind() {
    let split_ev = ProgressEvent {
        operation: OperationKind::Split,
        current_chunk: 0,
        total_chunks: 5,
        bytes_processed: 1024,
        total_bytes: 5120,
    };
    assert_eq!(split_ev.operation, OperationKind::Split);
    assert_eq!(split_ev.current_chunk, 0);
    assert_eq!(split_ev.total_chunks, 5);

    let merge_ev = ProgressEvent {
        operation: OperationKind::Merge,
        current_chunk: 1,
        total_chunks: 5,
        bytes_processed: 2048,
        total_bytes: 5120,
    };
    assert_eq!(merge_ev.operation, OperationKind::Merge);

    let verify_ev = ProgressEvent {
        operation: OperationKind::Verify,
        current_chunk: 2,
        total_chunks: 5,
        bytes_processed: 3072,
        total_bytes: 5120,
    };
    assert_eq!(verify_ev.operation, OperationKind::Verify);
    assert_ne!(split_ev.operation, merge_ev.operation);
}

#[test]
fn test_guard_deletes_file_on_drop_when_armed() {
    let dir = tempdir().unwrap();
    let tmp_path = dir.path().join("part-0000.chk.tmp");
    File::create(&tmp_path).unwrap();
    assert!(tmp_path.exists());

    {
        let _guard = TmpFileGuard::new(tmp_path.clone());
        assert!(tmp_path.exists());
    }

    assert!(
        !tmp_path.exists(),
        "Guard must delete temporary file on drop when armed"
    );
}

#[test]
fn test_guard_unwind_cleanup() {
    let dir = tempdir().unwrap();
    let tmp_path = dir.path().join("unwind.chk.tmp");
    File::create(&tmp_path).unwrap();
    assert!(tmp_path.exists());

    let path_clone = tmp_path.clone();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = TmpFileGuard::new(path_clone);
        panic!("simulated panic");
    }));

    assert!(
        !tmp_path.exists(),
        "Guard must delete temporary file when unwinding during a panic"
    );
}

#[test]
fn test_guard_empty_and_disarm() {
    let dir = tempdir().unwrap();
    let tmp_path = dir.path().join("keep.chk.tmp");
    File::create(&tmp_path).unwrap();
    assert!(tmp_path.exists());

    // Disarm path
    {
        let mut guard = TmpFileGuard::new(tmp_path.clone());
        guard.disarm();
    }
    assert!(
        tmp_path.exists(),
        "Disarmed guard must NOT delete the file on drop"
    );

    // Empty guard path
    {
        let mut empty_guard = TmpFileGuard::empty();
        assert!(empty_guard.path().is_none());
        empty_guard.disarm();
    }
    assert!(tmp_path.exists());
}

#[test]
fn test_guard_reset_unlinks_previous() {
    let dir = tempdir().unwrap();
    let file1 = dir.path().join("first.tmp");
    let file2 = dir.path().join("second.tmp");
    File::create(&file1).unwrap();
    File::create(&file2).unwrap();
    assert!(file1.exists());
    assert!(file2.exists());

    {
        let mut guard = TmpFileGuard::new(file1.clone());
        guard.reset(file2.clone());

        // file1 should be deleted when reset() is called
        assert!(
            !file1.exists(),
            "reset() must unlink previous armed file path"
        );
        assert!(file2.exists(), "reset() target file must still exist");
    }

    // After guard drops, file2 should be deleted
    assert!(
        !file2.exists(),
        "Dropping reset guard must delete the new armed file"
    );
}

#[test]
fn test_guard_declared_before_writer_drop_order() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test_lock.tmp");

    {
        // Guard declared FIRST
        let _guard = TmpFileGuard::new(file_path.clone());
        // Writer declared SECOND
        let mut writer = File::create(&file_path).unwrap();
        writer.write_all(b"sample data").unwrap();
        // In Rust, writer drops first (closing handle), then guard drops (deleting file).
        // On Windows, if guard dropped first, file deletion would fail with SharingViolation.
    }

    assert!(
        !file_path.exists(),
        "File must be deleted when guard is declared before file handle"
    );
}

#[test]
fn test_safe_parent_fallback() {
    assert_eq!(safe_parent(Path::new("a/b/c.txt")), Path::new("a/b"));
    assert_eq!(safe_parent(Path::new("file.txt")), Path::new("."));
    assert_eq!(safe_parent(Path::new("")), Path::new("."));
    assert_eq!(safe_parent(Path::new(".")), Path::new("."));
}

#[test]
fn test_make_tmp_path_appends_tmp() {
    assert_eq!(
        make_tmp_path(Path::new("archive.tar.gz")),
        PathBuf::from("archive.tar.gz.tmp")
    );
    assert_eq!(
        make_tmp_path(Path::new("part-0000.chk")),
        PathBuf::from("part-0000.chk.tmp")
    );
    assert_eq!(
        make_tmp_path(Path::new("manifest.json")),
        PathBuf::from("manifest.json.tmp")
    );
    assert_eq!(make_tmp_path(Path::new("file")), PathBuf::from("file.tmp"));
}

#[test]
fn test_validate_plain_filename() {
    // Valid plain filenames
    assert!(validate_plain_filename("archive.iso").is_ok());
    assert!(validate_plain_filename("data.tar.gz").is_ok());
    assert!(validate_plain_filename("my_file-123.bin").is_ok());
    assert!(validate_plain_filename("COM10.txt").is_ok()); // COM10 is not 1-9
    assert!(validate_plain_filename("contact.txt").is_ok()); // Starts with CON but not CON

    // Empty or dot names
    assert!(validate_plain_filename("").is_err());
    assert!(validate_plain_filename(".").is_err());
    assert!(validate_plain_filename("..").is_err());

    // Path separators & traversal
    assert!(validate_plain_filename("dir/file.txt").is_err());
    assert!(validate_plain_filename("dir\\file.txt").is_err());
    assert!(validate_plain_filename("../file.txt").is_err());
    assert!(validate_plain_filename("..\\file.txt").is_err());

    // Invalid characters
    for ch in ['<', '>', ':', '"', '/', '\\', '|', '?', '*'] {
        let name = format!("bad{ch}name.txt");
        assert!(
            validate_plain_filename(&name).is_err(),
            "Expected error for char '{ch}'"
        );
    }

    // Control characters (0..32)
    assert!(validate_plain_filename("control\x00char.txt").is_err());
    assert!(validate_plain_filename("control\x1fchar.txt").is_err());

    // Trailing dot or space
    assert!(validate_plain_filename("trailing.").is_err());
    assert!(validate_plain_filename("trailing ").is_err());
    assert!(validate_plain_filename("file.txt.").is_err());
    assert!(validate_plain_filename("file.txt ").is_err());

    // Windows reserved device names (checked before first dot, case-insensitive)
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    for name in reserved {
        let bare = name;
        let with_ext = format!("{name}.txt");
        let lower = format!("{}.dat", name.to_lowercase());
        assert!(
            validate_plain_filename(bare).is_err(),
            "Expected reserved name '{bare}' to be rejected"
        );
        assert!(
            validate_plain_filename(&with_ext).is_err(),
            "Expected reserved name '{with_ext}' to be rejected"
        );
        assert!(
            validate_plain_filename(&lower).is_err(),
            "Expected reserved name '{lower}' to be rejected"
        );
    }
}

#[test]
fn test_extract_filename() {
    assert_eq!(
        extract_filename(Path::new("dir/subdir/file.iso")).unwrap(),
        "file.iso"
    );
    assert_eq!(
        extract_filename(Path::new("dir\\subdir\\file.iso")).unwrap(),
        "file.iso"
    );
    assert_eq!(
        extract_filename(Path::new("dir/subdir\\file.iso")).unwrap(),
        "file.iso"
    );
    assert_eq!(
        extract_filename(Path::new("archive.tar.gz")).unwrap(),
        "archive.tar.gz"
    );

    // Unsafe or invalid filenames extracted must fail
    assert!(extract_filename(Path::new("dir/subdir/CON.txt")).is_err());
    assert!(extract_filename(Path::new("dir/subdir/bad:name.txt")).is_err());
    assert!(extract_filename(Path::new("dir/subdir/trailing. ")).is_err());
    assert!(extract_filename(Path::new("..")).is_err());
    assert!(extract_filename(Path::new("")).is_err());
    assert!(extract_filename(Path::new("/")).is_err());
    assert!(extract_filename(Path::new("\\")).is_err());
}

#[test]
fn test_clean_stale_tmp_files() {
    let dir = tempdir().unwrap();
    let bundle = dir.path();

    let stale1 = bundle.join("part-0000.chk.tmp");
    let stale2 = bundle.join("part-0001.chk.tmp");
    let stale_man = bundle.join("manifest.json.tmp");
    let valid_chk = bundle.join("part-0000.chk");
    let valid_man = bundle.join("manifest.json");
    let other_tmp = bundle.join("unrelated.txt.tmp");

    File::create(&stale1).unwrap();
    File::create(&stale2).unwrap();
    File::create(&stale_man).unwrap();
    File::create(&valid_chk).unwrap();
    File::create(&valid_man).unwrap();
    File::create(&other_tmp).unwrap();

    clean_stale_tmp_files(bundle).unwrap();

    assert!(!stale1.exists(), "part-0000.chk.tmp must be removed");
    assert!(!stale2.exists(), "part-0001.chk.tmp must be removed");
    assert!(!stale_man.exists(), "manifest.json.tmp must be removed");
    assert!(valid_chk.exists(), "part-0000.chk must NOT be removed");
    assert!(valid_man.exists(), "manifest.json must NOT be removed");
    assert!(other_tmp.exists(), "other .tmp must NOT be removed");
}

#[test]
fn test_clean_orphan_chunks() {
    let dir = tempdir().unwrap();
    let bundle = dir.path();

    let chk0 = bundle.join("part-0000.chk");
    let chk1 = bundle.join("part-0001.chk");
    let chk2 = bundle.join("part-0002.chk");
    let chk3 = bundle.join("part-0003.chk");
    let chk2_tmp = bundle.join("part-0002.chk.tmp");

    File::create(&chk0).unwrap();
    File::create(&chk1).unwrap();
    File::create(&chk2).unwrap();
    File::create(&chk3).unwrap();
    File::create(&chk2_tmp).unwrap();

    // Clean orphans starting from index 2
    clean_orphan_chunks(bundle, 2).unwrap();

    assert!(chk0.exists(), "part-0000.chk must remain");
    assert!(chk1.exists(), "part-0001.chk must remain");
    assert!(!chk2.exists(), "part-0002.chk must be removed (index >= 2)");
    assert!(!chk3.exists(), "part-0003.chk must be removed (index >= 2)");
    assert!(
        chk2_tmp.exists(),
        "part-0002.chk.tmp should not be removed by clean_orphan_chunks"
    );
}

#[test]
fn test_check_free_space() {
    let dir = tempdir().unwrap();
    let path = dir.path();

    // Normal space check should succeed on tempdir for small requirement
    let res = check_free_space(path, 1024, 0, 1024);
    assert!(res.is_ok());

    // Impossibly large space requirement should return InsufficientSpace
    let impossible_bytes = u64::MAX / 2;
    let res_huge = check_free_space(path, impossible_bytes, 0, 1024);
    match res_huge {
        Err(ChunkerError::InsufficientSpace {
            required,
            available: _,
            target,
        }) => {
            assert!(required >= impossible_bytes);
            assert_eq!(target, path.to_path_buf());
        }
        other => panic!("Expected InsufficientSpace error, got {other:?}"),
    }
}
