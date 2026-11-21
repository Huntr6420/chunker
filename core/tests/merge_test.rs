mod common;

use std::fs::File;
use std::io::{Cursor, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chunker_core::BUFFER_SIZE;
use chunker_core::error::ChunkerError;
use chunker_core::manifest::{ChunkInfo, Manifest, OriginalFileInfo, save_manifest_atomic};
use chunker_core::merge::{merge_bundle, merge_with_opener_unstable};
use chunker_core::progress::OperationKind;
use chunker_core::security::make_tmp_path;
use chunker_core::split::split_file;

use common::BoundedReader;

fn read_fixture(path: &Path) -> Vec<u8> {
    let mut file = File::open(path).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    buf
}

fn make_manifest(original_name: &str, data: &[u8], chunk_size: u64) -> (Manifest, Vec<Vec<u8>>) {
    let mut master = blake3::Hasher::new();
    master.update(data);
    let master_hash = master.finalize().to_hex().to_string();

    let mut chunks = Vec::new();
    let mut chunk_data = Vec::new();

    if data.is_empty() {
        // 0 chunks for empty data
    } else {
        for (idx, slice) in data.chunks(chunk_size as usize).enumerate() {
            let mut ch = blake3::Hasher::new();
            ch.update(slice);
            chunks.push(ChunkInfo {
                index: idx,
                file_name: format!("part-{:04}.chk", idx),
                size_bytes: slice.len() as u64,
                blake3: ch.finalize().to_hex().to_string(),
            });
            chunk_data.push(slice.to_vec());
        }
    }

    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".to_string(),
        original_file: OriginalFileInfo {
            name: original_name.to_string(),
            size_bytes: data.len() as u64,
            blake3: master_hash,
        },
        chunk_size_bytes: chunk_size,
        total_chunks: chunks.len(),
        chunks,
    };

    (manifest, chunk_data)
}

fn create_bundle_on_disk(dir: &Path, filename: &str, data: &[u8], chunk_size: u64) -> PathBuf {
    let source_path = dir.join(filename);
    std::fs::write(&source_path, data).unwrap();
    let summary = split_file(&source_path, dir, chunk_size, None, |_| {}).unwrap();
    std::fs::remove_file(&source_path).unwrap();
    summary.bundle_path
}

// -----------------------------------------------------------------------------
// Seam tests
// -----------------------------------------------------------------------------

#[test]
fn test_seam_round_trip_and_chunk_order() {
    let data = common::seeded_bytes(42, 250);
    let (manifest, chunk_data) = make_manifest("test.bin", &data, 100);
    assert_eq!(manifest.chunks.len(), 3);

    let mut writer = Vec::new();
    let mut events = Vec::new();

    let mut opener = |idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        let chunk = &chunk_data[idx];
        Ok((
            BoundedReader::new(Cursor::new(chunk.clone()), 10),
            chunk.len() as u64,
        ))
    };

    merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |ev| {
        events.push(ev)
    })
    .unwrap();

    assert_eq!(writer, data);
    assert_eq!(events.len(), 3);

    assert_eq!(events[0].current_chunk, 0);
    assert_eq!(events[0].bytes_processed, 100);
    assert_eq!(events[0].total_chunks, 3);
    assert_eq!(events[0].total_bytes, 250);
    assert_eq!(events[0].operation, OperationKind::Merge);

    assert_eq!(events[1].current_chunk, 1);
    assert_eq!(events[1].bytes_processed, 200);

    assert_eq!(events[2].current_chunk, 2);
    assert_eq!(events[2].bytes_processed, 250);
}

#[test]
fn test_seam_opener_error_propagates() {
    let data = b"hello world";
    let (manifest, _chunks) = make_manifest("test.bin", data, 100);

    let mut writer = Vec::new();
    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "opener denied",
        ))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::PermissionDenied);
            assert_eq!(err.to_string(), "opener denied");
        }
        other => panic!("expected ChunkerError::Io(PermissionDenied), got {other:?}"),
    }
}

#[test]
fn test_seam_reader_error_propagates() {
    let data = vec![1u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let mut writer = Vec::new();
    let mut opener = |_idx: usize| -> std::io::Result<(BoundedReader<common::FailingReader>, u64)> {
        let failing = common::FailingReader::new(
            vec![1u8; 100],
            1,
            std::io::Error::new(ErrorKind::BrokenPipe, "simulated reader error"),
        );
        Ok((BoundedReader::new(failing, 10), 100))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::BrokenPipe);
            assert_eq!(err.to_string(), "simulated reader error");
        }
        other => panic!("expected ChunkerError::Io(BrokenPipe), got {other:?}"),
    }
}

struct FailingWriter {
    fail_on_write: usize,
    write_count: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_count += 1;
        if self.write_count >= self.fail_on_write {
            Err(std::io::Error::new(
                ErrorKind::WriteZero,
                "simulated writer error",
            ))
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_seam_writer_error_propagates() {
    let data = vec![1u8; 100];
    let (manifest, chunks) = make_manifest("test.bin", &data, 100);

    let mut writer = FailingWriter {
        fail_on_write: 1,
        write_count: 0,
    };
    let mut opener = |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((BoundedReader::new(Cursor::new(chunks[0].clone()), 10), 100))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::WriteZero);
            assert_eq!(err.to_string(), "simulated writer error");
        }
        other => panic!("expected ChunkerError::Io(WriteZero), got {other:?}"),
    }
}

#[test]
fn test_seam_interrupted_retried() {
    let data = vec![42u8; 100];
    let (manifest, chunks) = make_manifest("test.bin", &data, 100);

    let mut writer = Vec::new();
    let mut opener =
        |_idx: usize| -> std::io::Result<(BoundedReader<common::InterruptingReader>, u64)> {
            let interrupting = common::InterruptingReader::new(chunks[0].clone(), 3);
            Ok((BoundedReader::new(interrupting, 10), 100))
        };

    merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {}).unwrap();
    assert_eq!(writer, data);
}

#[test]
fn test_seam_entry_cancellation() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let cancel_token = Arc::new(AtomicBool::new(true));
    let mut writer = Vec::new();
    let mut opens = 0;
    let mut events = 0;

    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        opens += 1;
        Ok((Cursor::new(vec![42u8; 100]), 100))
    };

    let res = merge_with_opener_unstable(
        &manifest,
        &mut opener,
        &mut writer,
        Some(&cancel_token),
        |_| events += 1,
    );

    match res {
        Err(ref err @ ChunkerError::Cancelled) => {
            assert_eq!(err.to_string(), "Operation cancelled by user");
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(opens, 0);
    assert_eq!(writer.len(), 0);
    assert_eq!(events, 0);
}

#[test]
fn test_seam_loop_top_cancellation() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let cancel_token = Arc::new(AtomicBool::new(false));
    let mut writer = Vec::new();
    let mut events = 0;

    let token_clone = Arc::clone(&cancel_token);
    let shared_calls = Arc::new(AtomicUsize::new(0));
    let sc_clone = Arc::clone(&shared_calls);

    let mut opener =
        move |_idx: usize| -> std::io::Result<(BoundedReader<common::ShortReadReader>, u64)> {
            let short = common::ShortReadReader::new(vec![42u8; 100], 40);
            Ok((
                BoundedReader::with_shared_calls(short, 10, Arc::clone(&sc_clone)),
                100,
            ))
        };

    let res = merge_with_opener_unstable(
        &manifest,
        &mut opener,
        &mut writer,
        Some(&cancel_token),
        |_| {
            events += 1;
            token_clone.store(true, Ordering::SeqCst);
        },
    );

    match res {
        Err(ref err @ ChunkerError::Cancelled) => {
            assert_eq!(err.to_string(), "Operation cancelled by user");
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(events, 1);
    assert_eq!(shared_calls.load(Ordering::SeqCst), 1);
    assert_eq!(writer.len(), 40);
}

#[test]
fn test_seam_metadata_mismatch() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let mut writer = Vec::new();
    let shared_calls = Arc::new(AtomicUsize::new(0));
    let sc = Arc::clone(&shared_calls);

    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((
            BoundedReader::with_shared_calls(Cursor::new(vec![42u8; 90]), 10, Arc::clone(&sc)),
            90,
        ))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 90);
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
    assert_eq!(shared_calls.load(Ordering::SeqCst), 0);
    assert_eq!(writer.len(), 0);
}

#[test]
fn test_seam_early_eof_short_reader() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let mut writer = Vec::new();
    let mut opener = |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        // Delivers 50 bytes then EOF; reports metadata length 100
        Ok((BoundedReader::new(Cursor::new(vec![42u8; 50]), 5), 100))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
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
                format!(
                    "{}",
                    ChunkerError::SizeMismatch {
                        file,
                        expected,
                        actual
                    }
                ),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 50 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
}

#[test]
fn test_seam_chunk_hash_failure() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let mut writer = Vec::new();
    let mut corrupted = vec![42u8; 100];
    corrupted[0] ^= 0xff;
    let corrupted_hash = blake3::hash(&corrupted).to_hex().to_string();

    let mut opener = |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((BoundedReader::new(Cursor::new(corrupted.clone()), 5), 100))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
    match res {
        Err(ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, manifest.chunks[0].blake3);
            assert_eq!(actual, corrupted_hash);
            assert_eq!(
                format!(
                    "{}",
                    ChunkerError::HashMismatch {
                        file: file.clone(),
                        expected: expected.clone(),
                        actual: actual.clone(),
                    }
                ),
                format!("Hash mismatch for part-0000.chk: expected {expected}, got {actual}")
            );
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }
}

#[test]
fn test_seam_master_hash_failure() {
    let data = vec![42u8; 100];
    let (mut manifest, chunks) = make_manifest("test.bin", &data, 100);
    let original_hash = manifest.original_file.blake3.clone();
    manifest.original_file.blake3 = "0".repeat(64);

    let mut writer = Vec::new();
    let mut opener = |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((BoundedReader::new(Cursor::new(chunks[0].clone()), 5), 100))
    };

    let res = merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {});
    match res {
        Err(ChunkerError::HashMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "test.bin");
            assert_eq!(expected, "0".repeat(64));
            assert_eq!(actual, original_hash);
            assert_eq!(
                format!(
                    "{}",
                    ChunkerError::HashMismatch {
                        file: file.clone(),
                        expected: expected.clone(),
                        actual: actual.clone(),
                    }
                ),
                format!(
                    "Hash mismatch for test.bin: expected {}, got {original_hash}",
                    "0".repeat(64)
                )
            );
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }
}

#[test]
fn test_seam_surplus_probe() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    let mut surplus_data = vec![42u8; 100];
    surplus_data.extend_from_slice(&[99u8; 5]); // 105 bytes total

    let shared_calls = Arc::new(AtomicUsize::new(0));
    let sc = Arc::clone(&shared_calls);
    let mut writer = Vec::new();
    let mut events = 0;

    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<Cursor<Vec<u8>>>, u64)> {
        Ok((
            BoundedReader::with_shared_calls(
                Cursor::new(surplus_data.clone()),
                10,
                Arc::clone(&sc),
            ),
            100,
        ))
    };

    let res =
        merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| events += 1);
    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 105);
            assert_eq!(
                format!(
                    "{}",
                    ChunkerError::SizeMismatch {
                        file,
                        expected,
                        actual
                    }
                ),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 105 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
    assert_eq!(shared_calls.load(Ordering::SeqCst), 2); // 1 payload read, 1 probe read
    assert_eq!(events, 1);
    assert_eq!(writer.len(), 100); // Only payload bytes written
}

#[test]
fn test_seam_surplus_probe_one_byte_with_more_available() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    // Custom reader: first read delivers 100 bytes, second read delivers 1 byte with more available
    struct OneSurplusReader {
        data: Vec<u8>,
        read_count: usize,
    }
    impl Read for OneSurplusReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.read_count += 1;
            if self.read_count == 1 {
                let to_copy = buf.len().min(100);
                buf[..to_copy].copy_from_slice(&self.data[..to_copy]);
                Ok(to_copy)
            } else if self.read_count == 2 {
                buf[0] = 0xee;
                Ok(1)
            } else {
                buf[0] = 0xff;
                Ok(1)
            }
        }
    }

    let shared_calls = Arc::new(AtomicUsize::new(0));
    let sc = Arc::clone(&shared_calls);
    let mut writer = Vec::new();
    let mut events = 0;

    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<OneSurplusReader>, u64)> {
        let r = OneSurplusReader {
            data: vec![42u8; 100],
            read_count: 0,
        };
        Ok((
            BoundedReader::with_shared_calls(r, 10, Arc::clone(&sc)),
            100,
        ))
    };

    let res =
        merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| events += 1);
    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 101);
            assert_eq!(
                format!(
                    "{}",
                    ChunkerError::SizeMismatch {
                        file,
                        expected,
                        actual
                    }
                ),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 101 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
    assert_eq!(shared_calls.load(Ordering::SeqCst), 2);
    assert_eq!(events, 1);
    assert_eq!(writer.len(), 100);
}

#[test]
fn test_seam_surplus_probe_precedence_over_cancellation() {
    let data = vec![42u8; 100];
    let (manifest, _chunks) = make_manifest("test.bin", &data, 100);

    struct OneSurplusReader {
        data: Vec<u8>,
        read_count: usize,
    }
    impl Read for OneSurplusReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.read_count += 1;
            if self.read_count == 1 {
                let to_copy = buf.len().min(100);
                buf[..to_copy].copy_from_slice(&self.data[..to_copy]);
                Ok(to_copy)
            } else if self.read_count == 2 {
                buf[0] = 0xee;
                Ok(1)
            } else {
                buf[0] = 0xff;
                Ok(1)
            }
        }
    }

    let cancel_token = Arc::new(AtomicBool::new(false));
    let token_clone = Arc::clone(&cancel_token);
    let mut writer = Vec::new();
    let mut events = 0;

    let mut opener = move |_idx: usize| -> std::io::Result<(BoundedReader<OneSurplusReader>, u64)> {
        let r = OneSurplusReader {
            data: vec![42u8; 100],
            read_count: 0,
        };
        Ok((BoundedReader::new(r, 10), 100))
    };

    let res = merge_with_opener_unstable(
        &manifest,
        &mut opener,
        &mut writer,
        Some(&cancel_token),
        |_| {
            events += 1;
            token_clone.store(true, Ordering::SeqCst);
        },
    );

    match res {
        Err(ChunkerError::SizeMismatch {
            file,
            expected,
            actual,
        }) => {
            assert_eq!(file, "part-0000.chk");
            assert_eq!(expected, 100);
            assert_eq!(actual, 101);
            assert_eq!(
                format!(
                    "{}",
                    ChunkerError::SizeMismatch {
                        file,
                        expected,
                        actual
                    }
                ),
                "Chunk size mismatch for part-0000.chk: expected 100 bytes, got 101 bytes"
            );
        }
        other => panic!("expected SizeMismatch, got {other:?}"),
    }
    assert_eq!(events, 1);
    assert_eq!(writer.len(), 100);
}

#[test]
fn test_seam_buffer_size_slices() {
    let size = 2 * BUFFER_SIZE as u64;
    let data = vec![77u8; size as usize];
    let (manifest, chunks) = make_manifest("test.bin", &data, size);

    #[derive(Clone)]
    struct SliceRecordingReader {
        data: Vec<u8>,
        pos: usize,
        slice_lengths: Arc<std::sync::Mutex<Vec<usize>>>,
    }

    impl Read for SliceRecordingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.slice_lengths.lock().unwrap().push(buf.len());
            if self.pos >= self.data.len() {
                Ok(0)
            } else {
                let to_copy = buf.len().min(self.data.len() - self.pos);
                buf[..to_copy].copy_from_slice(&self.data[self.pos..self.pos + to_copy]);
                self.pos += to_copy;
                Ok(to_copy)
            }
        }
    }

    let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rec_clone = Arc::clone(&recorded);

    let mut writer = Vec::new();
    let mut opener =
        move |_idx: usize| -> std::io::Result<(BoundedReader<SliceRecordingReader>, u64)> {
            let r = SliceRecordingReader {
                data: chunks[0].clone(),
                pos: 0,
                slice_lengths: Arc::clone(&rec_clone),
            };
            // Allow up to 600 calls so the 4 KiB mutation fails the slice assertion instead of call limit
            Ok((BoundedReader::new(r, 600), size))
        };

    merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| {}).unwrap();

    let slices = recorded.lock().unwrap().clone();
    assert_eq!(
        slices,
        vec![BUFFER_SIZE, BUFFER_SIZE, BUFFER_SIZE],
        "expected exactly three BUFFER_SIZE slices (two payload, one probe)"
    );
    assert_eq!(writer.len(), size as usize);
}

#[test]
fn test_seam_empty_bundle() {
    let (manifest, _chunks) = make_manifest("empty.bin", &[], 100);
    assert_eq!(manifest.chunks.len(), 0);

    let mut writer = Vec::new();
    let mut events = 0;
    let mut opens = 0;

    let mut opener = |_idx: usize| -> std::io::Result<(Cursor<Vec<u8>>, u64)> {
        opens += 1;
        Ok((Cursor::new(Vec::new()), 0))
    };

    merge_with_opener_unstable(&manifest, &mut opener, &mut writer, None, |_| events += 1).unwrap();

    assert_eq!(opens, 0);
    assert_eq!(writer.len(), 0);
    assert_eq!(events, 0);
}

// -----------------------------------------------------------------------------
// Real-file wrapper tests
// -----------------------------------------------------------------------------

#[test]
fn test_wrapper_round_trip() {
    let temp = common::make_temp_dir();
    let data = common::seeded_bytes(1234, 2 * 1024 * 1024 + 500);
    let bundle = create_bundle_on_disk(temp.path(), "data.bin", &data, 1024 * 1024);

    let out_dir = temp.path().join("output");
    std::fs::create_dir_all(&out_dir).unwrap();

    let dest = merge_bundle(&bundle, Some(&out_dir), false, None, |_| {}).unwrap();
    assert_eq!(dest, out_dir.join("data.bin"));

    let read_back = read_fixture(&dest);
    assert_eq!(read_back, data);
}

#[test]
fn test_wrapper_empty_bundle() {
    let temp = common::make_temp_dir();
    let bundle = create_bundle_on_disk(temp.path(), "empty.txt", &[], 1024 * 1024);

    let mut events = 0;
    let out_path = temp.path().join("merged_empty.txt");
    let dest = merge_bundle(&bundle, Some(&out_path), false, None, |_| events += 1).unwrap();

    assert_eq!(dest, out_path);
    assert_eq!(events, 0);
    assert!(dest.is_file());
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 0);
}

#[test]
fn test_wrapper_default_output_beside_bundle() {
    let temp = common::make_temp_dir();
    let data = b"placed beside bundle";
    let bundle = create_bundle_on_disk(temp.path(), "beside.bin", data, 1024 * 1024);

    let dest = merge_bundle(&bundle, None, false, None, |_| {}).unwrap();
    let expected = temp.path().join("beside.bin");
    assert_eq!(dest, expected);
    assert_eq!(read_fixture(&dest), data);
}

#[test]
fn test_wrapper_explicit_directory_output() {
    let temp = common::make_temp_dir();
    let data = b"explicit directory";
    let bundle = create_bundle_on_disk(temp.path(), "file.bin", data, 1024 * 1024);

    let target_dir = temp.path().join("target_dir");
    std::fs::create_dir_all(&target_dir).unwrap();

    let dest = merge_bundle(&bundle, Some(&target_dir), false, None, |_| {}).unwrap();
    assert_eq!(dest, target_dir.join("file.bin"));
    assert_eq!(read_fixture(&dest), data);
}

#[test]
fn test_wrapper_explicit_file_output() {
    let temp = common::make_temp_dir();
    let data = b"explicit file path";
    let bundle = create_bundle_on_disk(temp.path(), "orig.bin", data, 1024 * 1024);

    let explicit_file = temp.path().join("custom_name.dat");
    let dest = merge_bundle(&bundle, Some(&explicit_file), false, None, |_| {}).unwrap();

    assert_eq!(dest, explicit_file);
    assert_eq!(read_fixture(&dest), data);
}

#[test]
fn test_wrapper_bundle_not_a_directory() {
    let temp = common::make_temp_dir();
    let bundle_dir = create_bundle_on_disk(temp.path(), "empty.txt", &[], 1024 * 1024);
    let manifest_file = bundle_dir.join("manifest.json");
    assert!(manifest_file.is_file());

    let res = merge_bundle(&manifest_file, None, false, None, |_| {});
    match res {
        Err(ChunkerError::InvalidArgument(msg)) => {
            assert_eq!(
                msg,
                format!(
                    "Bundle path must be a directory: {}",
                    manifest_file.display()
                )
            );
            assert_eq!(
                format!("{}", ChunkerError::InvalidArgument(msg)),
                format!(
                    "Invalid argument: Bundle path must be a directory: {}",
                    manifest_file.display()
                )
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }

    // Assert no output or temp file created in bundle_dir or parent
    assert!(!temp.path().join("empty.txt").exists());
    assert!(!temp.path().join("empty.txt.tmp").exists());
}

#[test]
fn test_wrapper_entry_cancellation_before_directory_check() {
    let temp = common::make_temp_dir();
    let not_a_dir = temp.path().join("some_file.txt");
    std::fs::write(&not_a_dir, b"not a dir").unwrap();

    let cancel_token = Arc::new(AtomicBool::new(true));
    let res = merge_bundle(&not_a_dir, None, false, Some(&cancel_token), |_| {});
    match res {
        Err(ref err @ ChunkerError::Cancelled) => {
            assert_eq!(err.to_string(), "Operation cancelled by user");
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
}

#[test]
fn test_wrapper_missing_parent_one_level() {
    let temp = common::make_temp_dir();
    let bundle = create_bundle_on_disk(temp.path(), "data.bin", b"hello", 1024 * 1024);

    let nonexistent_parent = temp.path().join("missing_parent");
    let explicit_dest = nonexistent_parent.join("data.bin");

    let res = merge_bundle(&bundle, Some(&explicit_dest), false, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert_eq!(
                err.to_string(),
                format!(
                    "Output parent directory does not exist: {}",
                    nonexistent_parent.display()
                )
            );
            assert_eq!(
                format!("{}", ChunkerError::Io(err)),
                format!(
                    "I/O error: Output parent directory does not exist: {}",
                    nonexistent_parent.display()
                )
            );
        }
        other => panic!("expected Io(NotFound), got {other:?}"),
    }

    assert!(!nonexistent_parent.exists());
    assert!(!explicit_dest.exists());
}

#[test]
fn test_wrapper_missing_parent_two_levels() {
    let temp = common::make_temp_dir();
    let bundle = create_bundle_on_disk(temp.path(), "data.bin", b"hello", 1024 * 1024);

    let missing_level1 = temp.path().join("lvl1");
    let missing_level2 = missing_level1.join("lvl2");
    let explicit_dest = missing_level2.join("data.bin");

    let res = merge_bundle(&bundle, Some(&explicit_dest), false, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert_eq!(
                err.to_string(),
                format!(
                    "Output parent directory does not exist: {}",
                    missing_level2.display()
                )
            );
            assert_eq!(
                format!("{}", ChunkerError::Io(err)),
                format!(
                    "I/O error: Output parent directory does not exist: {}",
                    missing_level2.display()
                )
            );
        }
        other => panic!("expected Io(NotFound), got {other:?}"),
    }

    assert!(!missing_level1.exists());
    assert!(!missing_level2.exists());
    assert!(!explicit_dest.exists());
}

#[test]
fn test_wrapper_parent_is_a_file() {
    let temp = common::make_temp_dir();
    let bundle = create_bundle_on_disk(temp.path(), "data.bin", b"hello", 1024 * 1024);

    let parent_file = temp.path().join("parent_file.txt");
    std::fs::write(&parent_file, b"i am a file").unwrap();
    let explicit_dest = parent_file.join("data.bin");

    let res = merge_bundle(&bundle, Some(&explicit_dest), false, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert_eq!(
                err.to_string(),
                format!(
                    "Output parent directory does not exist: {}",
                    parent_file.display()
                )
            );
            assert_eq!(
                format!("{}", ChunkerError::Io(err)),
                format!(
                    "I/O error: Output parent directory does not exist: {}",
                    parent_file.display()
                )
            );
        }
        other => panic!("expected Io(NotFound), got {other:?}"),
    }
}

#[test]
fn test_wrapper_disk_space_order() {
    let temp = common::make_temp_dir();
    let huge_size = 100 * 1024 * 1024 * 1024 * 1024u64; // 100 TiB
    let manifest = Manifest {
        version: 1,
        hash_algorithm: "blake3".to_string(),
        original_file: OriginalFileInfo {
            name: "huge.bin".to_string(),
            size_bytes: huge_size,
            blake3: "0".repeat(64),
        },
        chunk_size_bytes: huge_size,
        total_chunks: 1,
        chunks: vec![ChunkInfo {
            index: 0,
            file_name: "part-0000.chk".to_string(),
            size_bytes: huge_size,
            blake3: "0".repeat(64),
        }],
    };

    let bundle_dir = temp.path().join("huge.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();
    save_manifest_atomic(&bundle_dir, &manifest).unwrap();

    let dest = temp.path().join("huge.bin");
    let tmp_path = make_tmp_path(&dest);
    // Put a directory at the expected .tmp path to detect if temp open was attempted
    std::fs::create_dir_all(&tmp_path).unwrap();

    let res = merge_bundle(&bundle_dir, Some(&dest), false, None, |_| {});
    match res {
        Err(
            ref err @ ChunkerError::InsufficientSpace {
                required,
                available,
                ref target,
            },
        ) => {
            assert_eq!(required, huge_size);
            assert!(available < required);
            assert_eq!(target, temp.path());
            let expected_msg = format!(
                "Insufficient disk space on {}: required {} bytes, available {} bytes",
                temp.path().display(),
                huge_size,
                available
            );
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected InsufficientSpace, got {other:?}"),
    }

    // Assert the directory at .tmp path was left untouched
    assert!(tmp_path.is_dir());
}

#[test]
fn test_wrapper_existing_destination_without_force_early() {
    let temp = common::make_temp_dir();
    let data = b"merged content";
    let bundle = create_bundle_on_disk(temp.path(), "conflict.bin", data, 1024 * 1024);

    let dest = temp.path().join("existing.bin");
    std::fs::write(&dest, b"existing content").unwrap();

    let mut events = 0;
    let res = merge_bundle(&bundle, Some(&dest), false, None, |_| events += 1);
    match res {
        Err(ref err @ ChunkerError::DestinationAlreadyExists(ref path)) => {
            assert_eq!(path, &dest);
            let expected_msg = format!(
                "Destination already exists: {}. Use --force to overwrite.",
                dest.display()
            );
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected DestinationAlreadyExists, got {other:?}"),
    }

    assert_eq!(events, 0);
    assert_eq!(read_fixture(&dest), b"existing content");
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_existing_destination_with_force() {
    let temp = common::make_temp_dir();
    let data = b"new merged content";
    let bundle = create_bundle_on_disk(temp.path(), "overwrite.bin", data, 1024 * 1024);

    let dest = temp.path().join("target.bin");
    std::fs::write(&dest, b"old content").unwrap();

    let res = merge_bundle(&bundle, Some(&dest), true, None, |_| {}).unwrap();
    assert_eq!(res, dest);
    assert_eq!(read_fixture(&dest), data);
}

#[test]
fn test_wrapper_late_overwrite_check() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 100];
    let bundle = create_bundle_on_disk(temp.path(), "late.bin", &data, 1024 * 1024);

    let dest = temp.path().join("late.bin");
    assert!(!dest.exists());

    let dest_clone = dest.clone();
    let res = merge_bundle(&bundle, Some(&dest), false, None, move |_ev| {
        // Create destination during progress
        std::fs::write(&dest_clone, b"created mid-flight").unwrap();
    });

    match res {
        Err(ref err @ ChunkerError::DestinationAlreadyExists(ref path)) => {
            assert_eq!(path, &dest);
            let expected_msg = format!(
                "Destination already exists: {}. Use --force to overwrite.",
                dest.display()
            );
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected DestinationAlreadyExists, got {other:?}"),
    }

    assert_eq!(read_fixture(&dest), b"created mid-flight");
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_rename_failure_preserves_dest_and_cleans_tmp() {
    let temp = common::make_temp_dir();
    let data = b"rename test data";
    let bundle = create_bundle_on_disk(temp.path(), "orig.bin", data, 1024 * 1024);

    let out_dir = temp.path().join("out_dir");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Resolved destination will be out_dir.join("orig.bin")
    let dest_dir = out_dir.join("orig.bin");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let nested_file = dest_dir.join("nested.txt");
    std::fs::write(&nested_file, b"do not overwrite directory").unwrap();

    // Derive expected error from the same rename on a scratch layout
    let scratch_src = temp.path().join("scratch.tmp");
    std::fs::write(&scratch_src, b"scratch").unwrap();
    let scratch_dest = temp.path().join("scratch_dir");
    std::fs::create_dir_all(&scratch_dest).unwrap();
    std::fs::write(scratch_dest.join("nested.txt"), b"scratch").unwrap();
    let scratch_err = std::fs::rename(&scratch_src, &scratch_dest)
        .expect_err("scratch rename over directory must fail");
    let expected_kind = scratch_err.kind();
    let expected_raw = scratch_err.raw_os_error();
    let expected_msg = format!("I/O error: {}", scratch_err);

    let res = merge_bundle(&bundle, Some(&out_dir), true, None, |_| {});
    match res {
        Err(ref err @ ChunkerError::Io(ref io_err)) => {
            assert_eq!(io_err.kind(), expected_kind);
            assert_eq!(io_err.raw_os_error(), expected_raw);
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected Io error from rename over non-empty directory, got {other:?}"),
    }

    // Preserved contents
    assert!(dest_dir.is_dir());
    assert_eq!(read_fixture(&nested_file), b"do not overwrite directory");
    // Temp cleaned up
    assert!(!make_tmp_path(&dest_dir).exists());
}

#[test]
fn test_wrapper_tmp_appending_multiple_dots() {
    let temp = common::make_temp_dir();
    let data = b"multi dot test";
    let bundle = create_bundle_on_disk(temp.path(), "archive.tar.gz", data, 1024 * 1024);

    let dest = temp.path().join("archive.tar.gz");
    let progress_called = Arc::new(AtomicBool::new(false));
    let pc_clone = Arc::clone(&progress_called);
    let tmp_existed = Arc::new(AtomicBool::new(false));
    let te_clone = Arc::clone(&tmp_existed);
    let dest_clone = dest.clone();
    let wrong_tmp = temp.path().join("archive.tar.tmp");
    let res = merge_bundle(&bundle, Some(&dest), false, None, move |_| {
        pc_clone.store(true, Ordering::SeqCst);
        if make_tmp_path(&dest_clone).exists() {
            te_clone.store(true, Ordering::SeqCst);
        }
        assert!(make_tmp_path(&dest_clone).exists());
        assert!(!wrong_tmp.exists());
    })
    .unwrap();
    assert!(progress_called.load(Ordering::SeqCst));
    assert!(tmp_existed.load(Ordering::SeqCst));
    assert_eq!(res, dest);
    assert_eq!(read_fixture(&dest), data);
}

#[test]
fn test_wrapper_missing_chunk() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 2 * 1024 * 1024];
    let bundle = create_bundle_on_disk(temp.path(), "missing_chunk.bin", &data, 1024 * 1024);

    let chunk1 = bundle.join("part-0001.chk");
    std::fs::remove_file(&chunk1).unwrap();

    let dest = temp.path().join("out.bin");
    let res = merge_bundle(&bundle, Some(&dest), false, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
        }
        other => panic!("expected Io(NotFound), got {other:?}"),
    }

    assert!(!dest.exists());
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_corrupt_chunk_absent_destination() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 1024 * 1024];
    let bundle = create_bundle_on_disk(temp.path(), "corrupt.bin", &data, 1024 * 1024);

    let chunk0 = bundle.join("part-0000.chk");
    let mut chunk_bytes = read_fixture(&chunk0);
    chunk_bytes[0] ^= 0xff;
    std::fs::write(&chunk0, &chunk_bytes).unwrap();

    let dest = temp.path().join("out.bin");
    let res = merge_bundle(&bundle, Some(&dest), false, None, |_| {});
    match res {
        Err(ChunkerError::HashMismatch { file, .. }) => {
            assert_eq!(file, "part-0000.chk");
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }

    assert!(!dest.exists());
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_corrupt_chunk_existing_destination_under_force() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 1024 * 1024];
    let bundle = create_bundle_on_disk(temp.path(), "corrupt_force.bin", &data, 1024 * 1024);

    let chunk0 = bundle.join("part-0000.chk");
    let mut chunk_bytes = read_fixture(&chunk0);
    chunk_bytes[0] ^= 0xff;
    std::fs::write(&chunk0, &chunk_bytes).unwrap();

    let dest = temp.path().join("out.bin");
    std::fs::write(&dest, b"preserve me!").unwrap();

    let res = merge_bundle(&bundle, Some(&dest), true, None, |_| {});
    match res {
        Err(ChunkerError::HashMismatch { file, .. }) => {
            assert_eq!(file, "part-0000.chk");
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }

    assert_eq!(read_fixture(&dest), b"preserve me!");
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_mid_merge_cancellation_no_destination() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 2 * 1024 * 1024];
    let bundle = create_bundle_on_disk(temp.path(), "mid_cancel.bin", &data, 1024 * 1024);

    let dest = temp.path().join("out.bin");
    let cancel_token = Arc::new(AtomicBool::new(false));
    let token_clone = Arc::clone(&cancel_token);

    let res = merge_bundle(
        &bundle,
        Some(&dest),
        false,
        Some(&cancel_token),
        move |_ev| {
            token_clone.store(true, Ordering::SeqCst);
        },
    );

    match res {
        Err(ref err @ ChunkerError::Cancelled) => {
            assert_eq!(err.to_string(), "Operation cancelled by user");
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    assert!(!dest.exists());
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_mid_merge_cancellation_with_force_and_existing_dest() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 2 * 1024 * 1024];
    let bundle = create_bundle_on_disk(temp.path(), "mid_force_cancel.bin", &data, 1024 * 1024);

    let dest = temp.path().join("out.bin");
    std::fs::write(&dest, b"initial file contents").unwrap();

    let cancel_token = Arc::new(AtomicBool::new(false));
    let token_clone = Arc::clone(&cancel_token);

    let res = merge_bundle(
        &bundle,
        Some(&dest),
        true,
        Some(&cancel_token),
        move |_ev| {
            token_clone.store(true, Ordering::SeqCst);
        },
    );

    match res {
        Err(ref err @ ChunkerError::Cancelled) => {
            assert_eq!(err.to_string(), "Operation cancelled by user");
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    assert_eq!(read_fixture(&dest), b"initial file contents");
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_final_cancellation_check() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 100];
    let bundle = create_bundle_on_disk(temp.path(), "final_cancel.bin", &data, 1024 * 1024);

    let dest = temp.path().join("out.bin");
    let cancel_token = Arc::new(AtomicBool::new(false));
    let token_clone = Arc::clone(&cancel_token);

    let res = merge_bundle(
        &bundle,
        Some(&dest),
        false,
        Some(&cancel_token),
        move |_ev| {
            token_clone.store(true, Ordering::SeqCst);
        },
    );

    match res {
        Err(ref err @ ChunkerError::Cancelled) => {
            assert_eq!(err.to_string(), "Operation cancelled by user");
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    assert!(!dest.exists());
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_precedence_late_overwrite_over_final_cancellation() {
    let temp = common::make_temp_dir();
    let data = vec![42u8; 100];
    let bundle = create_bundle_on_disk(temp.path(), "precedence.bin", &data, 1024 * 1024);

    let dest = temp.path().join("precedence.bin");
    assert!(!dest.exists());

    let cancel_token = Arc::new(AtomicBool::new(false));
    let token_clone = Arc::clone(&cancel_token);
    let dest_clone = dest.clone();

    let res = merge_bundle(
        &bundle,
        Some(&dest),
        false,
        Some(&cancel_token),
        move |_ev| {
            // Create destination AND set cancel token
            std::fs::write(&dest_clone, b"created before late check").unwrap();
            token_clone.store(true, Ordering::SeqCst);
        },
    );

    match res {
        Err(ref err @ ChunkerError::DestinationAlreadyExists(ref path)) => {
            assert_eq!(path, &dest);
            let expected_msg = format!(
                "Destination already exists: {}. Use --force to overwrite.",
                dest.display()
            );
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected DestinationAlreadyExists to win over Cancelled, got {other:?}"),
    }

    assert_eq!(read_fixture(&dest), b"created before late check");
    assert!(!make_tmp_path(&dest).exists());
}

#[test]
fn test_wrapper_tmp_hard_linked_to_dest_corrupt_bundle() {
    let temp = common::make_temp_dir();
    let valid_data = b"correct bundle data";
    let bundle = create_bundle_on_disk(temp.path(), "target.bin", valid_data, 1024 * 1024);

    // Corrupt part-0000.chk by copying valid bytes and changing one byte without changing length
    let chunk_path = bundle.join("part-0000.chk");
    let mut chunk_bytes = read_fixture(&chunk_path);
    chunk_bytes[0] ^= 0xFF;
    std::fs::write(&chunk_path, chunk_bytes).unwrap();

    let dest = temp.path().join("target.bin");
    let original_dest_bytes = b"original destination content that must not be lost";
    std::fs::write(&dest, original_dest_bytes).unwrap();

    let tmp_path = make_tmp_path(&dest);
    if let Err(e) = std::fs::hard_link(&dest, &tmp_path) {
        panic!("Failed to create hard link for test B1: {e}");
    }

    let mut events = 0;
    let res = merge_bundle(&bundle, Some(&dest), true, None, |_| events += 1);

    // Check destination bytes BEFORE checking the error so the first red run shows data loss directly
    assert_eq!(read_fixture(&dest), original_dest_bytes);

    // Expected refusal: ChunkerError::Io, ErrorKind::AlreadyExists, custom display
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

    assert!(tmp_path.exists());
    assert_eq!(read_fixture(&tmp_path), original_dest_bytes);
    assert_eq!(events, 0);
}

#[test]
fn test_wrapper_tmp_hard_linked_to_unrelated_file() {
    let temp = common::make_temp_dir();
    let bundle = create_bundle_on_disk(temp.path(), "dest.bin", b"bundle content", 1024 * 1024);

    let dest = temp.path().join("dest.bin");
    assert!(!dest.exists());

    let file_x = temp.path().join("unrelated_file.txt");
    let original_x_bytes = b"precious unrelated file contents";
    std::fs::write(&file_x, original_x_bytes).unwrap();

    let tmp_path = make_tmp_path(&dest);
    if let Err(e) = std::fs::hard_link(&file_x, &tmp_path) {
        panic!("Failed to create hard link for test B2: {e}");
    }

    let mut events = 0;
    let res = merge_bundle(&bundle, Some(&dest), false, None, |_| events += 1);

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

    assert!(file_x.exists());
    assert_eq!(read_fixture(&file_x), original_x_bytes);
    assert!(tmp_path.exists());
    assert_eq!(read_fixture(&tmp_path), original_x_bytes);
    assert!(!dest.exists());
    assert_eq!(events, 0);
}

#[test]
fn test_wrapper_tmp_stale_regular_file() {
    let temp = common::make_temp_dir();
    let bundle = create_bundle_on_disk(temp.path(), "dest.bin", b"bundle content", 1024 * 1024);

    let dest = temp.path().join("dest.bin");
    assert!(!dest.exists());

    let tmp_path = make_tmp_path(&dest);
    let junk_bytes = b"leftover stale junk bytes in tmp";
    std::fs::write(&tmp_path, junk_bytes).unwrap();

    let mut events = 0;
    let res = merge_bundle(&bundle, Some(&dest), false, None, |_| events += 1);

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

    assert!(tmp_path.exists());
    assert_eq!(read_fixture(&tmp_path), junk_bytes);
    assert!(!dest.exists());
    assert_eq!(events, 0);
}

#[test]
fn test_wrapper_tmp_is_directory() {
    let temp = common::make_temp_dir();

    // Scratch layout probe
    let scratch_dir = temp.path().join("scratch_dir");
    std::fs::create_dir_all(&scratch_dir).unwrap();
    let scratch_res = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch_dir);
    let scratch_err = scratch_res.expect_err("create_new on directory must fail");
    let scratch_kind = scratch_err.kind();
    let scratch_raw = scratch_err.raw_os_error();

    let bundle = create_bundle_on_disk(temp.path(), "dir_tmp.bin", b"bundle content", 1024 * 1024);
    let dest = temp.path().join("dir_tmp.bin");
    let tmp_path = make_tmp_path(&dest);
    std::fs::create_dir_all(&tmp_path).unwrap();
    let marker_file = tmp_path.join("marker.txt");
    let marker_bytes = b"marker file content inside temp directory";
    std::fs::write(&marker_file, marker_bytes).unwrap();

    let mut events = 0;
    let res = merge_bundle(&bundle, Some(&dest), false, None, |_| events += 1);

    match res {
        Err(ref err @ ChunkerError::Io(ref io_err)) => {
            assert_eq!(io_err.kind(), scratch_kind);
            if scratch_kind == std::io::ErrorKind::AlreadyExists {
                let expected_msg = format!(
                    "I/O error: Temporary file already exists: {}. If it is a leftover from an earlier run, remove it and retry.",
                    tmp_path.display()
                );
                assert_eq!(err.to_string(), expected_msg);
            } else {
                assert_eq!(io_err.raw_os_error(), scratch_raw);
                assert_eq!(err.to_string(), format!("I/O error: {}", scratch_err));
            }
        }
        other => panic!("expected Io({:?}), got {other:?}", scratch_kind),
    }

    assert!(tmp_path.is_dir());
    assert_eq!(read_fixture(&marker_file), marker_bytes);
    assert!(!dest.exists());
    assert_eq!(events, 0);
}
