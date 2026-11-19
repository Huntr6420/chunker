mod common;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chunker_core::MIN_CHUNK_SIZE;
use chunker_core::error::ChunkerError;
use chunker_core::manifest::{ChunkInfo, Manifest, OriginalFileInfo};
use chunker_core::progress::OperationKind;
use chunker_core::split::split_file;
use chunker_core::verify::{verify_bundle, verify_with_opener_unstable};

/// Reusable bounded reader wrapping any `Read` type that enforces a hard call bound.
struct BoundedReader<R> {
    inner: R,
    calls: usize,
    max_calls: usize,
}

impl<R: Read> BoundedReader<R> {
    fn new(inner: R, max_calls: usize) -> Self {
        Self {
            inner,
            calls: 0,
            max_calls,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.calls > self.max_calls {
            panic!(
                "BoundedReader call limit exceeded: {} > {}",
                self.calls, self.max_calls
            );
        }
        self.inner.read(buf)
    }
}

/// Helper to create a valid bundle on disk for testing.
fn create_test_bundle(output_dir: &Path, file_name: &str, data: &[u8], chunk_size: u64) -> PathBuf {
    let source_path = output_dir.join(file_name);
    std::fs::write(&source_path, data).unwrap();
    let summary = split_file(&source_path, output_dir, chunk_size, None, |_| {}).unwrap();
    summary.bundle_path
}

#[derive(Debug, PartialEq, Eq)]
enum EntryType {
    File(Vec<u8>),
    Directory,
    Symlink,
    Other,
}

/// Snapshot files in a bundle (entry name -> entry type / bytes) for byte-for-byte comparison.
/// Uses `symlink_metadata` (does not follow symlinks), reads file contents with `File::open` + `read_to_end`,
/// and panics on any enumeration or I/O error instead of skipping.
fn snapshot_bundle(bundle_dir: &Path) -> BTreeMap<String, EntryType> {
    let mut map = BTreeMap::new();
    let read_dir = std::fs::read_dir(bundle_dir)
        .unwrap_or_else(|e| panic!("failed to read directory {bundle_dir:?}: {e}"));
    for entry in read_dir {
        let entry = entry.unwrap_or_else(|e| panic!("directory entry enumeration error: {e}"));
        let path = entry.path();
        let name = path
            .file_name()
            .unwrap_or_else(|| panic!("missing file name for {path:?}"))
            .to_str()
            .unwrap_or_else(|| panic!("invalid utf8 filename: {path:?}"))
            .to_string();

        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("failed to get symlink_metadata for {path:?}: {e}"));

        let file_type = meta.file_type();
        let entry_type = if file_type.is_file() {
            let mut f =
                File::open(&path).unwrap_or_else(|e| panic!("failed to open file {path:?}: {e}"));
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes)
                .unwrap_or_else(|e| panic!("failed to read file {path:?}: {e}"));
            EntryType::File(bytes)
        } else if file_type.is_dir() {
            EntryType::Directory
        } else if file_type.is_symlink() {
            EntryType::Symlink
        } else {
            EntryType::Other
        };

        map.insert(name, entry_type);
    }
    map
}

#[test]
fn test_invalid_manifest_rejected_by_verify_bundle() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("corrupted.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Manifest with invalid hash algorithm
    let invalid_json = serde_json::json!({
        "version": 1,
        "hash_algorithm": "sha256", // must be blake3
        "original_file": {
            "name": "corrupted.bin",
            "size_bytes": 100,
            "blake3": "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        },
        "chunk_size_bytes": 100,
        "total_chunks": 1,
        "chunks": [
            {
                "index": 0,
                "file_name": "part-0000.chk",
                "size_bytes": 100,
                "blake3": "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
            }
        ]
    });
    std::fs::write(bundle_dir.join("manifest.json"), invalid_json.to_string()).unwrap();

    let res = verify_bundle(&bundle_dir, None, |_| {});
    match res {
        Err(ChunkerError::InvalidManifest(msg)) => {
            assert_eq!(msg, "Unsupported version or hash algorithm");
            assert_eq!(
                ChunkerError::InvalidManifest(msg).to_string(),
                "Invalid manifest: Unsupported version or hash algorithm"
            );
        }
        other => panic!("expected InvalidManifest, got {other:?}"),
    }
}

#[test]
fn test_missing_manifest_rejected_by_verify_bundle() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("missing_manifest.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let res = verify_bundle(&bundle_dir, None, |_| {});
    let expected_os_err = File::open(bundle_dir.join("manifest.json")).unwrap_err();
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert_eq!(
                ChunkerError::Io(err).to_string(),
                format!("I/O error: {}", expected_os_err)
            );
        }
        other => panic!("expected Io(NotFound), got {other:?}"),
    }
}

#[test]
fn test_missing_chunk_file_in_verify_bundle() {
    let temp = common::make_temp_dir();
    let data = common::seeded_bytes(101, 2 * MIN_CHUNK_SIZE as usize);
    let bundle_dir = create_test_bundle(temp.path(), "missing_chunk.bin", &data, MIN_CHUNK_SIZE);

    // Delete part-0001.chk
    let missing_chunk_path = bundle_dir.join("part-0001.chk");
    std::fs::remove_file(&missing_chunk_path).unwrap();

    let res = verify_bundle(&bundle_dir, None, |_| {});
    let expected_os_err = File::open(&missing_chunk_path).unwrap_err();
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert_eq!(
                ChunkerError::Io(err).to_string(),
                format!("I/O error: {}", expected_os_err)
            );
        }
        other => panic!("expected Io(NotFound), got {other:?}"),
    }
}

#[test]
fn test_truncated_chunk_rejected_by_metadata_check() {
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"test data").to_hex().to_string(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"test data").to_hex().to_string(),
        }],
    };

    struct NoReadReader;
    impl Read for NoReadReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            panic!("reader.read() must not be called when metadata check fails (bound 0)");
        }
    }

    // Opener reports metadata length of 50 bytes (expected 100)
    let mut opener =
        move |_idx: usize| -> std::io::Result<(NoReadReader, u64)> { Ok((NoReadReader, 50)) };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 50);
            assert_eq!(
                ChunkerError::SizeMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 50 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

#[test]
fn test_short_stream_with_matching_metadata() {
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"test data").to_hex().to_string(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"test data").to_hex().to_string(),
        }],
    };

    // Opener reports matching metadata (100 bytes), but stream only yields 50 bytes
    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((BoundedReader::new(Cursor::new(vec![0u8; 50]), 10), 100))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 50);
            assert_eq!(
                ChunkerError::SizeMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 50 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

#[test]
fn test_long_stream_with_matching_metadata() {
    let payload = vec![0u8; 100];
    let payload_hash = blake3::hash(&payload).to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: 100,
            blake3: payload_hash.clone(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: payload_hash,
        }],
    };

    // Reader yields 100 bytes initially, then returns 1 byte per read for 20 additional surplus reads.
    // Has a hard call bound of 50 calls; panics if exceeded.
    struct HardBoundedTrailingReader {
        head: Cursor<Vec<u8>>,
        surplus_remaining: usize,
        calls: usize,
        max_calls: usize,
    }
    impl Read for HardBoundedTrailingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.max_calls {
                panic!(
                    "hard call limit exceeded: {} > {}",
                    self.calls, self.max_calls
                );
            }
            let n = self.head.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if self.surplus_remaining > 0 && !buf.is_empty() {
                buf[0] = 0xee;
                self.surplus_remaining -= 1;
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    let mut opener = move |_idx: usize| -> std::io::Result<(HardBoundedTrailingReader, u64)> {
        Ok((
            HardBoundedTrailingReader {
                head: Cursor::new(payload.clone()),
                surplus_remaining: 20,
                calls: 0,
                max_calls: 50,
            },
            100,
        ))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 120);
            assert_eq!(
                ChunkerError::SizeMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 120 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

#[test]
fn test_cancellation_during_surplus_drain() {
    let payload = vec![0x42u8; 100];
    let payload_hash = blake3::hash(&payload).to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "drain_cancel.bin".into(),
            size_bytes: 100,
            blake3: payload_hash.clone(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: payload_hash,
        }],
    };

    struct SurplusCancellingReader {
        head: Cursor<Vec<u8>>,
        token: Arc<AtomicBool>,
        surplus_remaining: usize,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        max_calls: usize,
    }

    impl Read for SurplusCancellingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let call_idx = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call_idx > self.max_calls {
                panic!(
                    "SurplusCancellingReader call limit exceeded: {} > {}",
                    call_idx, self.max_calls
                );
            }
            let n = self.head.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if self.surplus_remaining > 0 && !buf.is_empty() {
                if call_idx == 2 {
                    self.token.store(true, Ordering::SeqCst);
                }
                buf[0] = 0xee;
                self.surplus_remaining -= 1;
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    let token = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let token_clone = token.clone();
    let calls_clone = calls.clone();

    let mut opener = move |_idx: usize| -> std::io::Result<(SurplusCancellingReader, u64)> {
        Ok((
            SurplusCancellingReader {
                head: Cursor::new(payload.clone()),
                token: token_clone.clone(),
                surplus_remaining: 20,
                calls: calls_clone.clone(),
                max_calls: 30,
            },
            100,
        ))
    };

    let mut progress_events = Vec::new();
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), |ev| {
        progress_events.push(ev);
    });

    let err = res.expect_err("expected SizeMismatch");
    assert_eq!(
        err.to_string(),
        "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 101 bytes"
    );
    match err {
        ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        } => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 101);
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }

    assert_eq!(calls.load(Ordering::SeqCst), 2, "must make exactly 2 reads");
    assert_eq!(
        progress_events.len(),
        1,
        "must fire exactly 1 payload progress event"
    );
    assert_eq!(progress_events[0].current_chunk, 0);
    assert_eq!(progress_events[0].bytes_processed, 100);
}

#[test]
fn test_same_size_corruption_detected_by_chunk_hash() {
    let data = b"valid chunk content with exact length 38";
    let valid_hash = blake3::hash(data).to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: data.len() as u64,
            blake3: valid_hash.clone(),
        },
        chunk_size_bytes: data.len() as u64,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: data.len() as u64,
            blake3: valid_hash.clone(),
        }],
    };

    // Corrupt one byte
    let mut corrupted_data = data.to_vec();
    corrupted_data[0] ^= 0xff;
    let corrupted_hash = blake3::hash(&corrupted_data).to_hex().to_string();

    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((
            BoundedReader::new(Cursor::new(corrupted_data.clone()), 10),
            corrupted_data.len() as u64,
        ))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    let err = res.expect_err("expected HashMismatch");
    assert_eq!(
        err.to_string(),
        format!("Hash mismatch for part-0000.chk: expected {valid_hash}, got {corrupted_hash}")
    );
    match err {
        ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        } => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, valid_hash);
            assert_eq!(actual, corrupted_hash);
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }
}

#[test]
fn test_reordered_chunks_detected() {
    let chunk0_data = b"chunk 0 data here...................";
    let chunk1_data = b"chunk 1 data here...................";
    let chunk0_hash = blake3::hash(chunk0_data).to_hex().to_string();
    let chunk1_hash = blake3::hash(chunk1_data).to_hex().to_string();

    let mut master = blake3::Hasher::new();
    master.update(chunk0_data);
    master.update(chunk1_data);
    let master_hash = master.finalize().to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "reordered.bin".into(),
            size_bytes: (chunk0_data.len() + chunk1_data.len()) as u64,
            blake3: master_hash,
        },
        chunk_size_bytes: chunk0_data.len() as u64,
        total_chunks: 2,
        chunks: vec![
            ChunkInfo {
                index: 0,
                file_name: "part-0000.chk".into(),
                size_bytes: chunk0_data.len() as u64,
                blake3: chunk0_hash.clone(),
            },
            ChunkInfo {
                index: 1,
                file_name: "part-0001.chk".into(),
                size_bytes: chunk1_data.len() as u64,
                blake3: chunk1_hash.clone(),
            },
        ],
    };

    // Opener returns chunk1 for index 0 and chunk0 for index 1
    let mut opener = move |idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        if idx == 0 {
            Ok((
                BoundedReader::new(Cursor::new(chunk1_data.to_vec()), 10),
                chunk1_data.len() as u64,
            ))
        } else {
            Ok((
                BoundedReader::new(Cursor::new(chunk0_data.to_vec()), 10),
                chunk0_data.len() as u64,
            ))
        }
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    match res {
        Err(ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, chunk0_hash);
            assert_eq!(actual, chunk1_hash);
            assert_eq!(
                ChunkerError::HashMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                format!(
                    "Hash mismatch for part-0000.chk: expected {chunk0_hash}, got {chunk1_hash}"
                )
            );
        }
        other => panic!("expected HashMismatch on part-0000.chk, got {other:?}"),
    }
}

#[test]
fn test_wrong_master_hash_with_valid_chunk_hashes() {
    let chunk0_data = b"chunk 0 data .......................";
    let chunk1_data = b"chunk 1 data .......................";
    let chunk0_hash = blake3::hash(chunk0_data).to_hex().to_string();
    let chunk1_hash = blake3::hash(chunk1_data).to_hex().to_string();

    let mut master = blake3::Hasher::new();
    master.update(chunk0_data);
    master.update(chunk1_data);
    let actual_master_hash = master.finalize().to_hex().to_string();

    let tampered_master_hash = "0000000000000000000000000000000000000000000000000000000000000000";

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: (chunk0_data.len() + chunk1_data.len()) as u64,
            blake3: tampered_master_hash.into(),
        },
        chunk_size_bytes: chunk0_data.len() as u64,
        total_chunks: 2,
        chunks: vec![
            ChunkInfo {
                index: 0,
                file_name: "part-0000.chk".into(),
                size_bytes: chunk0_data.len() as u64,
                blake3: chunk0_hash,
            },
            ChunkInfo {
                index: 1,
                file_name: "part-0001.chk".into(),
                size_bytes: chunk1_data.len() as u64,
                blake3: chunk1_hash,
            },
        ],
    };

    let mut opener = move |idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        if idx == 0 {
            Ok((
                BoundedReader::new(Cursor::new(chunk0_data.to_vec()), 10),
                chunk0_data.len() as u64,
            ))
        } else {
            Ok((
                BoundedReader::new(Cursor::new(chunk1_data.to_vec()), 10),
                chunk1_data.len() as u64,
            ))
        }
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    let err = res.expect_err("expected HashMismatch on master hash");
    assert_eq!(
        err.to_string(),
        format!(
            "Hash mismatch for test.bin: expected {tampered_master_hash}, got {actual_master_hash}"
        )
    );
    match err {
        ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        } => {
            assert_eq!(file, "test.bin");
            assert_eq!(expected, tampered_master_hash);
            assert_eq!(actual, actual_master_hash);
        }
        other => panic!("expected HashMismatch on master hash, got {other:?}"),
    }
}

#[test]
fn test_valid_empty_file_hash_succeeds() {
    let empty_hash = blake3::hash(b"").to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty.bin".into(),
            size_bytes: 0,
            blake3: empty_hash,
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };

    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        panic!("opener should not be called for empty file");
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    assert!(res.is_ok(), "empty file verification should succeed");
}

#[test]
fn test_invalid_empty_file_hash_rejected_in_manifest() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("empty_bad.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty_bad.bin".into(),
            size_bytes: 0,
            blake3: "1111111111111111111111111111111111111111111111111111111111111111".into(),
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };
    std::fs::write(
        bundle_dir.join("manifest.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();

    let res = verify_bundle(&bundle_dir, None, |_| {});
    match res {
        Err(ChunkerError::InvalidManifest(msg)) => {
            assert_eq!(msg, "Incorrect empty-file hash");
            assert_eq!(
                ChunkerError::InvalidManifest(msg).to_string(),
                "Invalid manifest: Incorrect empty-file hash"
            );
        }
        other => panic!("expected InvalidManifest, got {other:?}"),
    }
}

#[test]
fn test_interrupted_reads_retried_and_succeed() {
    let data = b"chunk with interrupted reads.............";
    let data_hash = blake3::hash(data).to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "interrupted.bin".into(),
            size_bytes: data.len() as u64,
            blake3: data_hash.clone(),
        },
        chunk_size_bytes: data.len() as u64,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: data.len() as u64,
            blake3: data_hash,
        }],
    };

    let mut opener =
        move |_idx: usize| -> std::io::Result<(BoundedReader<common::InterruptingReader>, u64)> {
            Ok((
                BoundedReader::new(common::InterruptingReader::new(data.to_vec(), 3), 10),
                data.len() as u64,
            ))
        };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    assert!(
        res.is_ok(),
        "interrupted reads should be retried and succeed"
    );
}

#[test]
fn test_io_error_propagation_from_opener() {
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"dummy").to_hex().to_string(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"dummy").to_hex().to_string(),
        }],
    };

    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "simulated permission denied",
        ))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::PermissionDenied);
            assert_eq!(
                ChunkerError::Io(err).to_string(),
                "I/O error: simulated permission denied"
            );
        }
        other => panic!("expected Io(PermissionDenied), got {other:?}"),
    }
}

#[test]
fn test_io_error_propagation_from_reader() {
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test.bin".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"dummy").to_hex().to_string(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"dummy").to_hex().to_string(),
        }],
    };

    let mut opener = |_idx: usize| -> std::io::Result<(BoundedReader<common::FailingReader>, u64)> {
        Ok((
            BoundedReader::new(
                common::FailingReader::new(
                    vec![0u8; 100],
                    1,
                    std::io::Error::new(ErrorKind::BrokenPipe, "simulated broken pipe"),
                ),
                5,
            ),
            100,
        ))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::BrokenPipe);
            assert_eq!(
                ChunkerError::Io(err).to_string(),
                "I/O error: simulated broken pipe"
            );
        }
        other => panic!("expected Io(BrokenPipe), got {other:?}"),
    }
}

#[test]
fn test_progress_reporting_across_reads_and_chunk_boundaries() {
    let chunk0_data = common::seeded_bytes(201, 150);
    let chunk1_data = common::seeded_bytes(202, 150);
    let chunk0_hash = blake3::hash(&chunk0_data).to_hex().to_string();
    let chunk1_hash = blake3::hash(&chunk1_data).to_hex().to_string();

    let mut master = blake3::Hasher::new();
    master.update(&chunk0_data);
    master.update(&chunk1_data);
    let master_hash = master.finalize().to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "progress.bin".into(),
            size_bytes: 300,
            blake3: master_hash,
        },
        chunk_size_bytes: 150,
        total_chunks: 2,
        chunks: vec![
            ChunkInfo {
                index: 0,
                file_name: "part-0000.chk".into(),
                size_bytes: 150,
                blake3: chunk0_hash,
            },
            ChunkInfo {
                index: 1,
                file_name: "part-0001.chk".into(),
                size_bytes: 150,
                blake3: chunk1_hash,
            },
        ],
    };

    // Use ShortReadReader with max 50 bytes per read (3 reads per chunk = 6 events total)
    let c0 = chunk0_data.clone();
    let c1 = chunk1_data.clone();
    let mut opener =
        move |idx: usize| -> std::io::Result<(BoundedReader<common::ShortReadReader>, u64)> {
            if idx == 0 {
                Ok((
                    BoundedReader::new(common::ShortReadReader::new(c0.clone(), 50), 10),
                    150,
                ))
            } else {
                Ok((
                    BoundedReader::new(common::ShortReadReader::new(c1.clone(), 50), 10),
                    150,
                ))
            }
        };

    let mut events = Vec::new();
    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |ev| {
        events.push(ev);
    });
    assert!(res.is_ok());

    assert_eq!(events.len(), 6);
    let processed_seq: Vec<u64> = events.iter().map(|ev| ev.bytes_processed).collect();
    assert_eq!(
        processed_seq,
        vec![50, 100, 150, 200, 250, 300],
        "progress bytes_processed must match exact sequence"
    );

    let mut prev_bytes = 0u64;
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.operation, OperationKind::Verify);
        assert_eq!(ev.total_chunks, 2);
        assert_eq!(ev.total_bytes, 300);
        assert!(
            ev.bytes_processed > prev_bytes,
            "bytes_processed must strictly increase"
        );
        prev_bytes = ev.bytes_processed;

        if i < 3 {
            assert_eq!(ev.current_chunk, 0);
        } else {
            assert_eq!(ev.current_chunk, 1);
        }
    }
    assert_eq!(events.last().unwrap().bytes_processed, 300);
    assert_eq!(events.last().unwrap().current_chunk, 1);
}

#[test]
fn test_empty_bundle_emits_no_chunk_progress() {
    let empty_hash = blake3::hash(b"").to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty.bin".into(),
            size_bytes: 0,
            blake3: empty_hash,
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };

    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        panic!("opener should not be called");
    };

    let mut events = Vec::new();
    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |ev| {
        events.push(ev);
    });
    assert!(res.is_ok());
    assert!(
        events.is_empty(),
        "empty bundle must emit no chunk progress events"
    );
}

#[test]
fn test_entry_cancellation_non_empty_bundle() {
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "cancel_entry.bin".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"dummy").to_hex().to_string(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: blake3::hash(b"dummy").to_hex().to_string(),
        }],
    };

    let mut opener = move |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        panic!("opener must not be called when cancelled at entry");
    };

    let token = Arc::new(AtomicBool::new(true)); // already cancelled at entry
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), |_| {});
    match res {
        Err(ChunkerError::Cancelled) => {
            assert_eq!(
                ChunkerError::Cancelled.to_string(),
                "Operation cancelled by user"
            );
        }
        other => panic!("expected Cancelled at entry, got {other:?}"),
    }
}

#[test]
fn test_entry_cancellation_empty_bundle() {
    let empty_hash = blake3::hash(b"").to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty_cancel.bin".into(),
            size_bytes: 0,
            blake3: empty_hash,
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };

    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        panic!("opener should not be called");
    };

    let token = Arc::new(AtomicBool::new(true)); // cancelled at entry
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), |_| {});
    match res {
        Err(ChunkerError::Cancelled) => {
            assert_eq!(
                ChunkerError::Cancelled.to_string(),
                "Operation cancelled by user"
            );
        }
        other => panic!("expected Cancelled at entry for empty bundle, got {other:?}"),
    }
}

#[test]
fn test_cancellation_at_top_of_read_loop_iteration() {
    let data = vec![0u8; 100];
    let hash = blake3::hash(&data).to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "cancel_loop.bin".into(),
            size_bytes: 100,
            blake3: hash.clone(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: hash,
        }],
    };

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reads_clone = reads.clone();

    struct CountingReader {
        inner: Cursor<Vec<u8>>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        calls: usize,
        max_calls: usize,
    }
    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.max_calls {
                panic!("CountingReader call limit exceeded: {}", self.calls);
            }
            let r = self.reads.fetch_add(1, Ordering::SeqCst);
            if r >= 1 {
                panic!("second read must not be called when read-loop check cancels");
            }
            let to_read = buf.len().min(50);
            self.inner.read(&mut buf[..to_read])
        }
    }

    let mut opener = move |_idx: usize| -> std::io::Result<(CountingReader, u64)> {
        Ok((
            CountingReader {
                inner: Cursor::new(vec![0u8; 100]),
                reads: reads_clone.clone(),
                calls: 0,
                max_calls: 5,
            },
            100,
        ))
    };

    let mut events = Vec::new();
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), |ev| {
        if ev.bytes_processed == 50 {
            token_clone.store(true, Ordering::SeqCst);
        }
        events.push(ev);
    });

    match res {
        Err(ChunkerError::Cancelled) => {
            assert_eq!(
                ChunkerError::Cancelled.to_string(),
                "Operation cancelled by user"
            );
        }
        other => panic!("expected Cancelled at top of read loop, got {other:?}"),
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "exactly 1 read must have occurred before cancellation stopped the loop"
    );
    assert_eq!(
        events.len(),
        1,
        "exactly 1 progress event must be emitted before loop-top cancellation"
    );
    assert_eq!(events[0].current_chunk, 0);
    assert_eq!(events[0].bytes_processed, 50);
}

#[test]
fn test_cancellation_at_final_success_check() {
    let data = vec![0u8; 100];
    let hash = blake3::hash(&data).to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "cancel_final.bin".into(),
            size_bytes: 100,
            blake3: hash.clone(),
        },
        chunk_size_bytes: 100,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: 100,
            blake3: hash,
        }],
    };

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    // Single read of 100 bytes. When progress fires for byte 100 (final read),
    // set token. The read loop has completed; hash check passes; final check cancels.
    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((BoundedReader::new(Cursor::new(vec![0u8; 100]), 10), 100))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), move |ev| {
        if ev.bytes_processed == 100 {
            token_clone.store(true, Ordering::SeqCst);
        }
    });

    match res {
        Err(ChunkerError::Cancelled) => {
            assert_eq!(
                ChunkerError::Cancelled.to_string(),
                "Operation cancelled by user"
            );
        }
        other => panic!("expected Cancelled at final success check, got {other:?}"),
    }
}

#[test]
fn test_error_detected_before_cancellation_takes_precedence() {
    let valid_data =
        b"chunk content 100 bytes padding ....................................................";
    let valid_hash = blake3::hash(valid_data).to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "precedence.bin".into(),
            size_bytes: valid_data.len() as u64,
            blake3: valid_hash.clone(),
        },
        chunk_size_bytes: valid_data.len() as u64,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: valid_data.len() as u64,
            blake3: valid_hash.clone(),
        }],
    };

    let mut corrupted = valid_data.to_vec();
    corrupted[0] ^= 0xaa; // Corrupted chunk data!
    let corrupted_hash = blake3::hash(&corrupted).to_hex().to_string();

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((
            BoundedReader::new(Cursor::new(corrupted.clone()), 10),
            corrupted.len() as u64,
        ))
    };

    // Callback cancels on the final read
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), move |ev| {
        if ev.bytes_processed == valid_data.len() as u64 {
            token_clone.store(true, Ordering::SeqCst);
        }
    });

    // HashMismatch must take precedence over Cancelled!
    match res {
        Err(ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, valid_hash);
            assert_eq!(actual, corrupted_hash);
            assert_eq!(
                ChunkerError::HashMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                format!(
                    "Hash mismatch for part-0000.chk: expected {valid_hash}, got {corrupted_hash}"
                )
            );
        }
        other => {
            panic!("expected HashMismatch to take precedence over Cancelled, got {other:?}")
        }
    }
}

#[test]
fn test_master_hash_error_before_cancellation_takes_precedence() {
    let chunk0_data = b"chunk 0 data .......................";
    let chunk1_data = b"chunk 1 data .......................";
    let chunk0_hash = blake3::hash(chunk0_data).to_hex().to_string();
    let chunk1_hash = blake3::hash(chunk1_data).to_hex().to_string();

    let mut master = blake3::Hasher::new();
    master.update(chunk0_data);
    master.update(chunk1_data);
    let actual_master_hash = master.finalize().to_hex().to_string();

    let tampered_master_hash = "0000000000000000000000000000000000000000000000000000000000000000";

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "test_master_prec.bin".into(),
            size_bytes: (chunk0_data.len() + chunk1_data.len()) as u64,
            blake3: tampered_master_hash.into(),
        },
        chunk_size_bytes: chunk0_data.len() as u64,
        total_chunks: 2,
        chunks: vec![
            ChunkInfo {
                index: 0,
                file_name: "part-0000.chk".into(),
                size_bytes: chunk0_data.len() as u64,
                blake3: chunk0_hash,
            },
            ChunkInfo {
                index: 1,
                file_name: "part-0001.chk".into(),
                size_bytes: chunk1_data.len() as u64,
                blake3: chunk1_hash,
            },
        ],
    };

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();
    let total_bytes = (chunk0_data.len() + chunk1_data.len()) as u64;

    let mut opener = move |idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        if idx == 0 {
            Ok((
                BoundedReader::new(Cursor::new(chunk0_data.to_vec()), 10),
                chunk0_data.len() as u64,
            ))
        } else {
            Ok((
                BoundedReader::new(Cursor::new(chunk1_data.to_vec()), 10),
                chunk1_data.len() as u64,
            ))
        }
    };

    // Callback cancels on the final event of the final chunk
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&token), move |ev| {
        if ev.bytes_processed == total_bytes {
            token_clone.store(true, Ordering::SeqCst);
        }
    });

    match res {
        Err(ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "test_master_prec.bin");
            assert_eq!(expected, tampered_master_hash);
            assert_eq!(actual, actual_master_hash);
            assert_eq!(
                ChunkerError::HashMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                format!(
                    "Hash mismatch for test_master_prec.bin: expected {tampered_master_hash}, got {actual_master_hash}"
                )
            );
        }
        other => panic!(
            "expected HashMismatch for master hash to take precedence over Cancelled, got {other:?}"
        ),
    }
}

#[test]
fn test_verify_bundle_on_disk_preserves_files_byte_for_byte_success() {
    let temp = common::make_temp_dir();
    let data = common::seeded_bytes(301, 2 * MIN_CHUNK_SIZE as usize);
    let bundle_dir = create_test_bundle(temp.path(), "preserve_success.bin", &data, MIN_CHUNK_SIZE);

    let snapshot_before = snapshot_bundle(&bundle_dir);
    assert!(!snapshot_before.is_empty());

    let res = verify_bundle(&bundle_dir, None, |_| {});
    assert!(res.is_ok(), "verify_bundle on valid bundle should succeed");

    let snapshot_after = snapshot_bundle(&bundle_dir);
    assert_eq!(
        snapshot_before, snapshot_after,
        "bundle files must remain 100% byte-for-byte identical after successful verification"
    );
}

#[test]
fn test_verify_bundle_on_disk_preserves_files_byte_for_byte_failure() {
    let temp = common::make_temp_dir();
    let data = common::seeded_bytes(302, 2 * MIN_CHUNK_SIZE as usize);
    let bundle_dir = create_test_bundle(temp.path(), "preserve_fail.bin", &data, MIN_CHUNK_SIZE);

    // Corrupt part-0001.chk on disk
    let chunk1_path = bundle_dir.join("part-0001.chk");
    let mut chunk1_bytes = Vec::new();
    File::open(&chunk1_path)
        .unwrap()
        .read_to_end(&mut chunk1_bytes)
        .unwrap();
    let original_hash = blake3::hash(&chunk1_bytes).to_hex().to_string();
    chunk1_bytes[10] ^= 0x55;
    let corrupted_hash = blake3::hash(&chunk1_bytes).to_hex().to_string();
    std::fs::write(&chunk1_path, &chunk1_bytes).unwrap();

    let snapshot_before = snapshot_bundle(&bundle_dir);

    let res = verify_bundle(&bundle_dir, None, |_| {});
    match res {
        Err(ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0001.chk");
            assert_eq!(expected, original_hash);
            assert_eq!(actual, corrupted_hash);
            assert_eq!(
                ChunkerError::HashMismatch {
                    file,
                    expected,
                    actual,
                }
                .to_string(),
                format!(
                    "Hash mismatch for part-0001.chk: expected {original_hash}, got {corrupted_hash}"
                )
            );
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }

    let snapshot_after = snapshot_bundle(&bundle_dir);
    assert_eq!(
        snapshot_before, snapshot_after,
        "bundle files must remain 100% byte-for-byte identical even after failed verification"
    );
}

#[test]
fn test_verify_bundle_on_read_only_files_succeeds() {
    let temp = common::make_temp_dir();
    let data = common::seeded_bytes(401, 2 * MIN_CHUNK_SIZE as usize);
    let bundle_dir = create_test_bundle(temp.path(), "readonly.bin", &data, MIN_CHUNK_SIZE);

    let mut paths_to_restore = Vec::new();
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_file() {
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            perm.set_readonly(true);
            std::fs::set_permissions(&p, perm).unwrap();
            paths_to_restore.push(p);
        }
    }

    struct PermGuard(Vec<PathBuf>);
    impl Drop for PermGuard {
        #[allow(clippy::permissions_set_readonly_false)]
        fn drop(&mut self) {
            for p in &self.0 {
                if let Ok(meta) = std::fs::metadata(p) {
                    let mut perm = meta.permissions();
                    perm.set_readonly(false);
                    let _ = std::fs::set_permissions(p, perm);
                }
            }
        }
    }
    let _guard = PermGuard(paths_to_restore);

    // Explicitly prove in the test that OpenOptions read(true).write(true) on a read-only chunk fails
    let chunk0_path = bundle_dir.join("part-0000.chk");
    let write_res = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&chunk0_path);
    assert!(
        write_res.is_err(),
        "read-only file fixture MUST deny write open on this platform"
    );
    assert_eq!(
        write_res.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );

    let res = verify_bundle(&bundle_dir, None, |_| {});
    assert!(
        res.is_ok(),
        "verify_bundle must succeed on read-only files: {res:?}"
    );
}

#[test]
fn test_buffer_size_is_at_least_one_mebibyte() {
    let chunk_size = 2 * 1024 * 1024u64; // 2 MiB
    let data = vec![0x42u8; chunk_size as usize];
    let chunk_hash = blake3::hash(&data).to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "two_mib.bin".into(),
            size_bytes: chunk_size,
            blake3: chunk_hash.clone(),
        },
        chunk_size_bytes: chunk_size,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".into(),
            size_bytes: chunk_size,
            blake3: chunk_hash,
        }],
    };

    let recorded_lens = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded_lens_clone = recorded_lens.clone();

    struct BufferInspectionReader {
        inner: Cursor<Vec<u8>>,
        recorded_lens: Arc<std::sync::Mutex<Vec<usize>>>,
        calls: usize,
        max_calls: usize,
    }
    impl Read for BufferInspectionReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.max_calls {
                panic!("BufferInspectionReader call limit exceeded: {}", self.calls);
            }
            self.recorded_lens.lock().unwrap().push(buf.len());
            self.inner.read(buf)
        }
    }

    let mut opener = move |_idx: usize| -> std::io::Result<(BufferInspectionReader, u64)> {
        Ok((
            BufferInspectionReader {
                inner: Cursor::new(data.clone()),
                recorded_lens: recorded_lens_clone.clone(),
                calls: 0,
                max_calls: 10,
            },
            chunk_size,
        ))
    };

    let res = verify_with_opener_unstable(&manifest, &mut opener, None, |_| {});
    assert!(res.is_ok(), "verification of 2 MiB chunk should succeed");

    let lens = recorded_lens.lock().unwrap().clone();
    assert!(
        !lens.is_empty(),
        "at least one read must have been performed"
    );
    assert_eq!(
        lens[0],
        chunker_core::BUFFER_SIZE,
        "first read buffer length must be exactly BUFFER_SIZE (1 MiB)"
    );
}

#[test]
fn test_verify_bundle_on_disk_entry_cancellation_precedes_manifest_load() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("empty_corrupt.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty_corrupt.bin".into(),
            size_bytes: 0,
            blake3: "2222222222222222222222222222222222222222222222222222222222222222".into(),
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };
    std::fs::write(
        bundle_dir.join("manifest.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();

    // 1. With pre-set cancel token: entry check in verify_bundle MUST return Cancelled before loading manifest
    let cancel_token = Arc::new(AtomicBool::new(true));
    let res = verify_bundle(&bundle_dir, Some(&cancel_token), |_| {});
    match res {
        Err(ChunkerError::Cancelled) => {
            assert_eq!(
                ChunkerError::Cancelled.to_string(),
                "Operation cancelled by user"
            );
        }
        other => panic!("expected Cancelled with pre-set token, got {other:?}"),
    }

    // 2. With clear token (None): proceeds to load manifest and fails with InvalidManifest
    let res_clear = verify_bundle(&bundle_dir, None, |_| {});
    match res_clear {
        Err(ChunkerError::InvalidManifest(msg)) => {
            assert_eq!(msg, "Incorrect empty-file hash");
            assert_eq!(
                ChunkerError::InvalidManifest(msg).to_string(),
                "Invalid manifest: Incorrect empty-file hash"
            );
        }
        other => panic!("expected InvalidManifest with clear token, got {other:?}"),
    }
}

#[test]
fn test_verify_bundle_on_disk_valid_empty_bundle_succeeds() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("empty_valid.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let empty_hash = blake3::hash(b"").to_hex().to_string();
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "empty_valid.bin".into(),
            size_bytes: 0,
            blake3: empty_hash,
        },
        chunk_size_bytes: 1024 * 1024,
        total_chunks: 0,
        chunks: vec![],
    };
    std::fs::write(
        bundle_dir.join("manifest.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();

    let mut events = Vec::new();
    let res = verify_bundle(&bundle_dir, None, |ev| events.push(ev));
    assert!(
        res.is_ok(),
        "valid empty bundle verification on disk must succeed: {res:?}"
    );
    assert!(
        events.is_empty(),
        "empty bundle on disk must emit no progress events"
    );
}

#[test]
fn test_seam_cancellation_at_opener_for_second_chunk() {
    let chunk0_data = vec![0x11u8; 100];
    let chunk1_data = vec![0x22u8; 100];
    let chunk0_hash = blake3::hash(&chunk0_data).to_hex().to_string();
    let chunk1_hash = blake3::hash(&chunk1_data).to_hex().to_string();

    let mut master = blake3::Hasher::new();
    master.update(&chunk0_data);
    master.update(&chunk1_data);
    let master_hash = master.finalize().to_hex().to_string();

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".into(),
        original_file: OriginalFileInfo {
            name: "cancel_opener_second.bin".into(),
            size_bytes: 200,
            blake3: master_hash,
        },
        chunk_size_bytes: 100,
        total_chunks: 2,
        chunks: vec![
            ChunkInfo {
                index: 0,
                file_name: "part-0000.chk".into(),
                size_bytes: 100,
                blake3: chunk0_hash,
            },
            ChunkInfo {
                index: 1,
                file_name: "part-0001.chk".into(),
                size_bytes: 100,
                blake3: chunk1_hash,
            },
        ],
    };

    let cancel_token = Arc::new(AtomicBool::new(false));
    let token_clone = cancel_token.clone();

    let chunk0_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let chunk1_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let c0_reads = chunk0_reads.clone();
    let c1_reads = chunk1_reads.clone();

    struct ReadTrackingReader {
        inner: Cursor<Vec<u8>>,
        read_counter: Arc<std::sync::atomic::AtomicUsize>,
        calls: usize,
        max_calls: usize,
    }
    impl Read for ReadTrackingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.max_calls {
                panic!("ReadTrackingReader call limit exceeded: {}", self.calls);
            }
            self.read_counter.fetch_add(1, Ordering::SeqCst);
            self.inner.read(buf)
        }
    }

    let mut opener = move |idx: usize| -> std::io::Result<(ReadTrackingReader, u64)> {
        match idx {
            0 => Ok((
                ReadTrackingReader {
                    inner: Cursor::new(chunk0_data.clone()),
                    read_counter: c0_reads.clone(),
                    calls: 0,
                    max_calls: 10,
                },
                100,
            )),
            1 => {
                // Opener for chunk index 1 (the second chunk) sets the cancel flag!
                token_clone.store(true, Ordering::SeqCst);
                Ok((
                    ReadTrackingReader {
                        inner: Cursor::new(chunk1_data.clone()),
                        read_counter: c1_reads.clone(),
                        calls: 0,
                        max_calls: 10,
                    },
                    100,
                ))
            }
            _ => unreachable!(),
        }
    };

    let mut events = Vec::new();
    let res = verify_with_opener_unstable(&manifest, &mut opener, Some(&cancel_token), |ev| {
        events.push(ev);
    });

    match res {
        Err(ChunkerError::Cancelled) => {
            assert_eq!(
                ChunkerError::Cancelled.to_string(),
                "Operation cancelled by user"
            );
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    assert_eq!(
        chunk1_reads.load(Ordering::SeqCst),
        0,
        "second chunk (index 1) reader must never be read"
    );
    assert_eq!(
        chunk0_reads.load(Ordering::SeqCst),
        2,
        "first chunk (index 0) must have exactly 2 reads (1 data read + 1 EOF drain probe)"
    );
    assert_eq!(
        events.len(),
        1,
        "exactly 1 progress event must be emitted (from the first chunk)"
    );
    assert_eq!(events[0].current_chunk, 0);
    assert_eq!(events[0].bytes_processed, 100);
}
