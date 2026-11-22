use tempfile::tempdir;

use chunker_core::error::ChunkerError;
use chunker_core::manifest::{
    ChunkInfo, Manifest, OriginalFileInfo, load_manifest, resolve_bundle_and_manifest,
    save_manifest_atomic, validate_hex_hash, validate_manifest,
};

fn dummy_hash(byte: u8) -> String {
    format!("{:02x}", byte).repeat(32)
}

fn create_valid_manifest() -> Manifest {
    Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "archive.iso".into(),
            size_bytes: 1500,
            blake3: dummy_hash(1),
        },
        chunk_size_bytes: 1000,
        total_chunks: 2,
        chunks: vec![
            ChunkInfo {
                index: 0,
                file_name: "part-0000.chk".into(),
                size_bytes: 1000,
                blake3: dummy_hash(2),
            },
            ChunkInfo {
                index: 1,
                file_name: "part-0001.chk".into(),
                size_bytes: 500,
                blake3: dummy_hash(3),
            },
        ],
    }
}

#[test]
fn test_valid_manifest() {
    let manifest = create_valid_manifest();
    assert!(validate_manifest(&manifest).is_ok());
}

#[test]
fn test_zero_byte_file_manifest() {
    let empty_hash = blake3::hash(b"").to_hex().to_string();
    let zero_manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty.dat".into(),
            size_bytes: 0,
            blake3: empty_hash.clone(),
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };
    assert!(validate_manifest(&zero_manifest).is_ok());

    // Bad hash for 0-byte file
    let mut bad_hash = zero_manifest.clone();
    bad_hash.original_file.blake3 = dummy_hash(9);
    assert!(matches!(
        validate_manifest(&bad_hash),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("empty-file hash")
    ));

    // 0-byte file with non-zero chunks
    let mut non_zero_chunks = zero_manifest.clone();
    non_zero_chunks.total_chunks = 1;
    non_zero_chunks.chunks = vec![ChunkInfo {
        index: 0,
        file_name: "part-0000.chk".into(),
        size_bytes: 0,
        blake3: empty_hash,
    }];
    assert!(matches!(
        validate_manifest(&non_zero_chunks),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("0-byte file must have 0 chunks")
    ));
}

#[test]
fn test_unsupported_version_or_algorithm() {
    let mut manifest = create_valid_manifest();
    manifest.version = 2;
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Unsupported version")
    ));

    let mut manifest = create_valid_manifest();
    manifest.hash_algorithm = "sha256".into();
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Unsupported version")
    ));
}

#[test]
fn test_validate_hex_hash_strict_lowercase() {
    let valid = dummy_hash(0xab);
    assert!(validate_hex_hash(&valid).is_ok());

    // Too short (63 chars)
    let short = &valid[..63];
    assert!(validate_hex_hash(short).is_err());

    // Too long (65 chars)
    let long = format!("{}a", valid);
    assert!(validate_hex_hash(&long).is_err());

    // Uppercase hex must be rejected
    let upper = valid.to_ascii_uppercase();
    assert!(validate_hex_hash(&upper).is_err());

    // Invalid non-hex characters
    let mut invalid_char = valid.clone();
    invalid_char.pop();
    invalid_char.push('g');
    assert!(validate_hex_hash(&invalid_char).is_err());
}

#[test]
fn test_chunk_size_validation() {
    // 0 chunk size must be rejected
    let mut manifest = create_valid_manifest();
    manifest.chunk_size_bytes = 0;
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("chunk_size_bytes must be positive")
    ));

    // Positive sub-MiB chunk sizes are accepted in manifest validation
    let mut sub_mib = create_valid_manifest();
    sub_mib.chunk_size_bytes = 500;
    sub_mib.original_file.size_bytes = 500;
    sub_mib.total_chunks = 1;
    sub_mib.chunks = vec![ChunkInfo {
        index: 0,
        file_name: "part-0000.chk".into(),
        size_bytes: 500,
        blake3: dummy_hash(1),
    }];
    assert!(validate_manifest(&sub_mib).is_ok());
}

#[test]
fn test_chunk_counts_mismatch() {
    let mut manifest = create_valid_manifest();
    manifest.total_chunks = 3; // actual is 2
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("total_chunks does not match chunks array length")
    ));
}

#[test]
fn test_chunk_index_gap_and_filename() {
    // Index gap
    let mut manifest = create_valid_manifest();
    manifest.chunks[1].index = 2; // Should be 1
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Chunk index gap at 1")
    ));

    // Filename mismatch
    let mut manifest = create_valid_manifest();
    manifest.chunks[0].file_name = "part-0001.chk".into();
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Chunk filename mismatch")
    ));
}

#[test]
fn test_chunk_sizes_mismatch() {
    // Non-final chunk size mismatch
    let mut manifest = create_valid_manifest();
    manifest.chunks[0].size_bytes = 999;
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Non-final chunk 0 does not match")
    ));

    // Final chunk size 0
    let mut manifest = create_valid_manifest();
    manifest.chunks[1].size_bytes = 0;
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Invalid final chunk size")
    ));

    // Final chunk size > chunk_size_bytes
    let mut manifest = create_valid_manifest();
    manifest.chunks[1].size_bytes = 1001;
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Invalid final chunk size")
    ));

    // Sum does not match original_file.size_bytes
    let mut manifest = create_valid_manifest();
    manifest.original_file.size_bytes = 2000; // sum is 1500
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Sum of chunk sizes (1500) does not match")
    ));
}

#[test]
fn test_checked_overflow_in_chunk_sizes() {
    let mut manifest = create_valid_manifest();
    manifest.chunk_size_bytes = u64::MAX;
    manifest.chunks[0].size_bytes = u64::MAX;
    manifest.chunks[1].size_bytes = 1;
    manifest.original_file.size_bytes = u64::MAX;
    assert!(matches!(
        validate_manifest(&manifest),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("overflowed u64")
    ));
}

#[test]
fn test_reject_unsafe_manifest_original_name() {
    let unsafe_names = [
        "../etc/passwd",
        "..\\windows\\system32",
        "dir/file.iso",
        "dir\\file.iso",
        "CON.txt",
        "com1.tar.gz",
        "file<name>.txt",
        "trailing. ",
        "trailing.",
        "",
        ".",
        "..",
    ];

    for name in unsafe_names {
        let mut manifest = create_valid_manifest();
        manifest.original_file.name = name.into();
        assert!(
            matches!(
                validate_manifest(&manifest),
                Err(ChunkerError::InvalidFilename(_))
            ),
            "Expected InvalidFilename for unsafe name '{name}'"
        );
    }
}

#[test]
fn test_deny_unknown_json_fields() {
    // Unknown field at root
    let root_unknown = r#"{
        "version": 1,
        "hash_algorithm": "blake3",
        "original_file": {
            "name": "archive.iso",
            "size_bytes": 1000,
            "blake3": "4b825dc642cb6eb9a060e54b3c579e8c4809a6f2202f5d1e2f5b6b1f5f8aca92"
        },
        "chunk_size_bytes": 1000,
        "total_chunks": 1,
        "chunks": [
            {
                "index": 0,
                "file_name": "part-0000.chk",
                "size_bytes": 1000,
                "blake3": "7a3f5c9e2b8104d4e3a890123456789abcdef0123456789abcdef0123456789a"
            }
        ],
        "unexpected_field": true
    }"#;
    assert!(serde_json::from_str::<Manifest>(root_unknown).is_err());

    // Unknown field in original_file
    let nested_unknown = r#"{
        "version": 1,
        "hash_algorithm": "blake3",
        "original_file": {
            "name": "archive.iso",
            "size_bytes": 1000,
            "blake3": "4b825dc642cb6eb9a060e54b3c579e8c4809a6f2202f5d1e2f5b6b1f5f8aca92",
            "extra": 123
        },
        "chunk_size_bytes": 1000,
        "total_chunks": 1,
        "chunks": []
    }"#;
    assert!(serde_json::from_str::<Manifest>(nested_unknown).is_err());

    // Unknown field in chunk
    let chunk_unknown = r#"{
        "index": 0,
        "file_name": "part-0000.chk",
        "size_bytes": 1000,
        "blake3": "7a3f5c9e2b8104d4e3a890123456789abcdef0123456789abcdef0123456789a",
        "crc32": "abcdef12"
    }"#;
    assert!(serde_json::from_str::<ChunkInfo>(chunk_unknown).is_err());
}

#[test]
fn test_save_manifest_atomic_and_load() {
    let dir = tempdir().unwrap();
    let bundle_dir = dir.path().join("archive.iso.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let manifest = create_valid_manifest();
    let manifest_path = save_manifest_atomic(&bundle_dir, &manifest).unwrap();

    assert_eq!(manifest_path, bundle_dir.join("manifest.json"));
    assert!(manifest_path.exists());
    assert!(!bundle_dir.join("manifest.json.tmp").exists());

    // Load from bundle dir
    let loaded_from_dir = load_manifest(&bundle_dir).unwrap();
    assert_eq!(loaded_from_dir, manifest);

    // Load from manifest file path
    let loaded_from_file = load_manifest(&manifest_path).unwrap();
    assert_eq!(loaded_from_file, manifest);

    // Invalid manifest should fail to save and not leave .tmp
    let mut invalid_manifest = manifest.clone();
    invalid_manifest.version = 99;
    let err = save_manifest_atomic(&bundle_dir, &invalid_manifest);
    assert!(err.is_err());
    assert!(!bundle_dir.join("manifest.json.tmp").exists());
}

// --- Part 1 Characterization Tests ---

#[test]
fn test_characterization_unknown_fields_control_and_error_message() {
    let control_json = r#"{
        "version": 1,
        "hash_algorithm": "blake3",
        "original_file": {
            "name": "archive.iso",
            "size_bytes": 1000,
            "blake3": "4b825dc642cb6eb9a060e54b3c579e8c4809a6f2202f5d1e2f5b6b1f5f8aca92"
        },
        "chunk_size_bytes": 1000,
        "total_chunks": 1,
        "chunks": [
            {
                "index": 0,
                "file_name": "part-0000.chk",
                "size_bytes": 1000,
                "blake3": "7a3f5c9e2b8104d4e3a890123456789abcdef0123456789abcdef0123456789a"
            }
        ]
    }"#;

    // Control passes
    let parsed: Result<Manifest, _> = serde_json::from_str(control_json);
    assert!(parsed.is_ok());

    // Root unknown field
    let root_unknown = control_json.replace(
        "\"version\": 1,",
        "\"version\": 1, \"unknown_root_field\": 123,",
    );
    let root_err = serde_json::from_str::<Manifest>(&root_unknown)
        .unwrap_err()
        .to_string();
    assert!(root_err.contains("unknown field"));
    assert!(root_err.contains("unknown_root_field"));

    // original_file unknown field
    let orig_unknown = control_json.replace(
        "\"name\": \"archive.iso\",",
        "\"name\": \"archive.iso\", \"unknown_orig_field\": 456,",
    );
    let orig_err = serde_json::from_str::<Manifest>(&orig_unknown)
        .unwrap_err()
        .to_string();
    assert!(orig_err.contains("unknown field"));
    assert!(orig_err.contains("unknown_orig_field"));

    // chunk unknown field
    let chunk_unknown = control_json.replace(
        "\"index\": 0,",
        "\"index\": 0, \"unknown_chunk_field\": 789,",
    );
    let chunk_err = serde_json::from_str::<Manifest>(&chunk_unknown)
        .unwrap_err()
        .to_string();
    assert!(chunk_err.contains("unknown field"));
    assert!(chunk_err.contains("unknown_chunk_field"));
}

#[test]
fn test_characterization_load_manifest_error_variants() {
    let dir = tempdir().unwrap();

    // 1. Missing file -> ChunkerError::Io
    let missing_path = dir.path().join("does_not_exist");
    let res = load_manifest(&missing_path);
    assert!(matches!(res, Err(ChunkerError::Io(_))));

    // 2. Malformed JSON -> ChunkerError::Manifest
    let malformed_path = dir.path().join("malformed.json");
    std::fs::write(&malformed_path, b"{ not json }").unwrap();
    let res = load_manifest(&malformed_path);
    assert!(matches!(res, Err(ChunkerError::Manifest(_))));

    // 3. Truncated JSON -> ChunkerError::Manifest
    let truncated_path = dir.path().join("truncated.json");
    std::fs::write(&truncated_path, b"{\"version\": 1, ").unwrap();
    let res = load_manifest(&truncated_path);
    assert!(matches!(res, Err(ChunkerError::Manifest(_))));

    // 4. Valid JSON followed by trailing garbage -> ChunkerError::Manifest
    let control_json = r#"{
        "version": 1,
        "hash_algorithm": "blake3",
        "original_file": {
            "name": "archive.iso",
            "size_bytes": 1000,
            "blake3": "4b825dc642cb6eb9a060e54b3c579e8c4809a6f2202f5d1e2f5b6b1f5f8aca92"
        },
        "chunk_size_bytes": 1000,
        "total_chunks": 1,
        "chunks": [
            {
                "index": 0,
                "file_name": "part-0000.chk",
                "size_bytes": 1000,
                "blake3": "7a3f5c9e2b8104d4e3a890123456789abcdef0123456789abcdef0123456789a"
            }
        ]
    }"#;
    let trailing_path = dir.path().join("trailing.json");
    std::fs::write(&trailing_path, format!("{control_json} trailing garbage")).unwrap();
    let res = load_manifest(&trailing_path);
    assert!(matches!(res, Err(ChunkerError::Manifest(_))));

    // 5. On-disk manifest with an unknown field -> ChunkerError::Manifest
    let unknown_field_path = dir.path().join("unknown.json");
    let unknown_json =
        control_json.replace("\"version\": 1,", "\"version\": 1, \"extra_prop\": true,");
    std::fs::write(&unknown_field_path, unknown_json).unwrap();
    let res = load_manifest(&unknown_field_path);
    assert!(matches!(res, Err(ChunkerError::Manifest(_))));

    // 6. Non-JSON binary file passed directly -> ChunkerError::Manifest, no panic
    let binary_path = dir.path().join("binary_not_json.bin");
    std::fs::write(&binary_path, b"\x01\x02\x03\x04\x05\x06").unwrap();
    let res = load_manifest(&binary_path);
    assert!(matches!(res, Err(ChunkerError::Manifest(_))));
}

#[test]
fn test_characterization_chunk_hashes_uppercase_and_short() {
    // Uppercase chunk hash -> InvalidManifest
    let mut manifest_upper = create_valid_manifest();
    manifest_upper.chunks[0].blake3 = dummy_hash(0xab).to_ascii_uppercase();
    assert!(matches!(
        validate_manifest(&manifest_upper),
        Err(ChunkerError::InvalidManifest(_))
    ));

    // Short chunk hash -> InvalidManifest
    let mut manifest_short = create_valid_manifest();
    manifest_short.chunks[0].blake3 = dummy_hash(0xab)[..63].to_string();
    assert!(matches!(
        validate_manifest(&manifest_short),
        Err(ChunkerError::InvalidManifest(_))
    ));
}

#[test]
fn test_characterization_size_gt_zero_chunks_empty() {
    let mut m = create_valid_manifest();
    m.original_file.size_bytes = 1000;
    m.total_chunks = 0;
    m.chunks = vec![];
    assert!(matches!(
        validate_manifest(&m),
        Err(ChunkerError::InvalidManifest(msg)) if msg.contains("Sum of chunk sizes")
    ));
}

#[test]
fn test_characterization_failed_save_leaves_existing_manifest_identical() {
    let dir = tempdir().unwrap();
    let bundle_dir = dir.path().join("bundle.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let valid_manifest = create_valid_manifest();
    let manifest_path = save_manifest_atomic(&bundle_dir, &valid_manifest).unwrap();
    let bytes_before = std::fs::read(&manifest_path).unwrap();

    // Attempt save with invalid manifest
    let mut invalid = valid_manifest.clone();
    invalid.version = 99; // invalid version
    let res = save_manifest_atomic(&bundle_dir, &invalid);
    assert!(res.is_err());

    let tmp_path = bundle_dir.join("manifest.json.tmp");
    assert!(
        !tmp_path.exists(),
        "manifest.json.tmp must not exist after failed save"
    );

    let bytes_after = std::fs::read(&manifest_path).unwrap();
    assert_eq!(
        bytes_before, bytes_after,
        "Existing manifest.json must remain byte-identical after failed save"
    );
}

#[test]
fn test_characterization_rename_failure_cleans_up_tmp() {
    let dir = tempdir().unwrap();
    let bundle_dir = dir.path().join("bundle.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Make manifest.json a non-empty directory containing a file
    let blocker_dir = bundle_dir.join("manifest.json");
    std::fs::create_dir_all(&blocker_dir).unwrap();
    std::fs::write(blocker_dir.join("blocker.txt"), b"block").unwrap();

    let valid_manifest = create_valid_manifest();

    // 1. Prove that the manifest passes validation (failure is not in validation)
    assert!(validate_manifest(&valid_manifest).is_ok());

    // 2. Prove that bundle_dir is writable and manifest.json.tmp can be created (failure is not in File::create)
    let tmp_path = bundle_dir.join("manifest.json.tmp");
    assert!(!tmp_path.exists());
    {
        let test_file = std::fs::File::create(&tmp_path);
        assert!(
            test_file.is_ok(),
            "tmp file creation must succeed in bundle_dir"
        );
    }
    std::fs::remove_file(&tmp_path).unwrap();

    // 3. Execute save_manifest_atomic: it validates, creates & writes tmp, but fails specifically at rename
    let res = save_manifest_atomic(&bundle_dir, &valid_manifest);

    let err = res.expect_err("Save should fail when destination is a non-empty directory");
    match err {
        ChunkerError::Io(io_err) => {
            #[cfg(windows)]
            {
                // On Windows, rename of a file over an existing directory fails with PermissionDenied (ERROR_ACCESS_DENIED, OS error 5)
                assert_eq!(io_err.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(io_err.raw_os_error(), Some(5));
            }
            #[cfg(not(windows))]
            {
                // On non-Windows platforms, renaming a file over a non-empty directory returns an I/O error (e.g. EISDIR / ENOTEMPTY)
                let _ = io_err;
            }
        }
        other => panic!("Expected ChunkerError::Io from rename failure, got {other:?}"),
    }

    // 4. Prove that the blocker directory and its contents were untouched
    assert!(blocker_dir.is_dir());
    assert_eq!(
        std::fs::read(blocker_dir.join("blocker.txt")).unwrap(),
        b"block"
    );

    // 5. Prove that the temporary file was cleaned up by TmpFileGuard
    assert!(
        !tmp_path.exists(),
        "manifest.json.tmp must be cleaned up on rename failure"
    );

    // 6. Prove that once the rename blocker is removed, the exact same save operation succeeds
    std::fs::remove_file(blocker_dir.join("blocker.txt")).unwrap();
    std::fs::remove_dir(&blocker_dir).unwrap();
    let retry_res = save_manifest_atomic(&bundle_dir, &valid_manifest);
    assert!(
        retry_res.is_ok(),
        "Save must succeed once rename destination is no longer blocked"
    );
    assert!(bundle_dir.join("manifest.json").is_file());
}

#[test]
fn test_resolve_bundle_and_manifest_directory_named_manifest_json() {
    let dir = tempdir().unwrap();
    let bundle_dir_named_manifest = dir.path().join("manifest.json");
    std::fs::create_dir(&bundle_dir_named_manifest).unwrap();
    assert!(bundle_dir_named_manifest.is_dir());

    let (bundle_dir, manifest_path) =
        resolve_bundle_and_manifest(&bundle_dir_named_manifest).unwrap();
    assert_eq!(bundle_dir, bundle_dir_named_manifest);
    assert_eq!(
        manifest_path,
        bundle_dir_named_manifest.join("manifest.json")
    );
}

fn read_file_content(path: &std::path::Path) -> Vec<u8> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    buf
}

#[test]
fn test_manifest_save_refuses_occupied_tmp() {
    let dir = tempdir().unwrap();
    let bundle_dir = dir.path().join("bundle.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let manifest1 = create_valid_manifest();
    let manifest_path =
        save_manifest_atomic(&bundle_dir, &manifest1).expect("initial save should succeed");
    let initial_manifest_bytes = read_file_content(&manifest_path);

    let tmp_path = bundle_dir.join("manifest.json.tmp");
    let occupied_bytes = b"pre-existing occupied manifest tmp content".to_vec();
    std::fs::write(&tmp_path, &occupied_bytes).unwrap();
    assert!(tmp_path.is_file());

    // Attempt to save a DIFFERENT valid manifest
    let mut manifest2 = manifest1.clone();
    manifest2.original_file.name = "different_name.iso".to_string();

    let res = save_manifest_atomic(&bundle_dir, &manifest2);

    match res {
        Err(ref err @ ChunkerError::Io(ref io_err)) => {
            assert_eq!(io_err.kind(), std::io::ErrorKind::AlreadyExists);
            let expected_msg = format!(
                "I/O error: Temporary file already exists: {}. If it is a leftover from an earlier run, remove it and retry.",
                tmp_path.display()
            );
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected Io(AlreadyExists), got {other:?}"),
    }

    assert!(manifest_path.is_file());
    assert_eq!(read_file_content(&manifest_path), initial_manifest_bytes);
    assert!(tmp_path.is_file());
    assert_eq!(read_file_content(&tmp_path), occupied_bytes);
}

#[test]
fn test_characterization_invalid_utf8_file() {
    let dir = tempdir().unwrap();
    let invalid_utf8_path = dir.path().join("invalid_utf8.json");
    // Invalid UTF-8 byte sequence
    std::fs::write(&invalid_utf8_path, b"\xFF\xFE\xFD").unwrap();

    // In Part 1, read_to_string produced ChunkerError::Io.
    // In Part 2 (now), serde_json::from_reader parses directly and reports syntax/UTF-8 decoding errors as ChunkerError::Manifest.
    let res = load_manifest(&invalid_utf8_path);
    assert!(
        matches!(res, Err(ChunkerError::Manifest(_))),
        "Expected ChunkerError::Manifest for invalid UTF-8 under parse_manifest/from_reader in Part 2, got {:?}",
        res
    );
}
