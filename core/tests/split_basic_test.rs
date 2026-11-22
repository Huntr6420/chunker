mod common;

use std::io::{Cursor, ErrorKind, Read, Seek};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chunker_core::error::ChunkerError;
use chunker_core::manifest::{load_manifest, validate_manifest};
use chunker_core::progress::ProgressEvent;
use chunker_core::split::{SplitSummary, split_file, split_with_reader_unstable};
use chunker_core::{BUFFER_SIZE, MIN_CHUNK_SIZE};

#[test]
fn test_public_sub_minimum_chunk_size_rejected() {
    let temp = common::make_temp_dir();
    let input_path = temp.path().join("source.bin");
    std::fs::write(&input_path, b"some content").unwrap();

    // Less than MIN_CHUNK_SIZE (1 MiB)
    let res = split_file(&input_path, temp.path(), MIN_CHUNK_SIZE - 1, None, |_| {});
    match res {
        Err(ChunkerError::InvalidArgument(msg)) => {
            assert!(
                msg.contains("MIN_CHUNK_SIZE") || msg.contains("chunk size"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }

    // Zero chunk size
    let res_zero = split_file(&input_path, temp.path(), 0, None, |_| {});
    assert!(matches!(res_zero, Err(ChunkerError::InvalidArgument(_))));
}

#[test]
fn test_public_missing_input_file() {
    let temp = common::make_temp_dir();
    let missing_path = temp.path().join("does_not_exist.bin");

    let res = split_file(&missing_path, temp.path(), MIN_CHUNK_SIZE, None, |_| {});
    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected ChunkerError::Io(NotFound), got {other:?}"),
    }
}

#[test]
fn test_seam_zero_chunk_and_buffer_sizes_rejected() {
    let temp = common::make_temp_dir();
    let mut reader = Cursor::new(b"hello world");

    // Zero chunk size
    let res_chunk = split_with_reader_unstable(
        &mut reader,
        "test.bin",
        11,
        0,
        1024,
        temp.path(),
        None,
        |_| {},
    );
    assert!(matches!(res_chunk, Err(ChunkerError::InvalidArgument(_))));

    // Zero buffer size
    let res_buf = split_with_reader_unstable(
        &mut reader,
        "test.bin",
        11,
        1024,
        0,
        temp.path(),
        None,
        |_| {},
    );
    assert!(matches!(res_buf, Err(ChunkerError::InvalidArgument(_))));
}

#[test]
fn test_seam_unsafe_original_filename_rejected() {
    let temp = common::make_temp_dir();
    let mut reader = Cursor::new(b"data");

    for bad_name in &["../escape.bin", "CON", "bad:name", "trailing. ", ""] {
        let res = split_with_reader_unstable(
            &mut reader,
            bad_name,
            4,
            1024,
            1024,
            temp.path(),
            None,
            |_| {},
        );
        assert!(
            matches!(res, Err(ChunkerError::InvalidFilename(_))),
            "expected InvalidFilename for '{bad_name}', got {res:?}"
        );
    }
}

#[test]
fn test_split_creates_nested_output_and_bundle_directory() {
    let temp = common::make_temp_dir();
    let nested_output = temp.path().join("nested").join("deep").join("output");
    let mut reader = Cursor::new(b"sample bytes");

    let summary = split_with_reader_unstable(
        &mut reader,
        "archive.dat",
        12,
        1024,
        1024,
        &nested_output,
        None,
        |_| {},
    )
    .expect("split should create nested output directory");

    assert_eq!(
        summary,
        SplitSummary {
            bundle_path: nested_output.join("archive.dat.chunks"),
            reused_chunks: 0,
        }
    );
    assert!(summary.bundle_path.is_dir());
    assert!(summary.bundle_path.join("manifest.json").is_file());
    assert!(summary.bundle_path.join("part-0000.chk").is_file());
}

#[test]
fn test_startup_removes_stale_manifest_and_stale_tmp_files() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("target.iso.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Place stale files
    let stale_manifest = bundle_dir.join("manifest.json");
    std::fs::write(&stale_manifest, b"{ stale json }").unwrap();

    let stale_chunk_tmp = bundle_dir.join("part-0000.chk.tmp");
    std::fs::write(&stale_chunk_tmp, b"stale chunk tmp").unwrap();

    let stale_manifest_tmp = bundle_dir.join("manifest.json.tmp");
    std::fs::write(&stale_manifest_tmp, b"stale manifest tmp").unwrap();

    // Stale tmp with an index the split never writes (split only writes chunk 0)
    let stale_unwritten_tmp = bundle_dir.join("part-0050.chk.tmp");
    std::fs::write(&stale_unwritten_tmp, b"stale unwritten chunk tmp").unwrap();

    let unrelated = bundle_dir.join("notes.txt");
    std::fs::write(&unrelated, b"keep this file").unwrap();

    let mut reader = Cursor::new(b"new fresh data");
    let summary = split_with_reader_unstable(
        &mut reader,
        "target.iso",
        14,
        1024,
        1024,
        temp.path(),
        None,
        |_| {},
    )
    .expect("split should succeed");

    assert_eq!(summary.bundle_path, bundle_dir);
    // Unrelated file is preserved
    assert!(unrelated.is_file());
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep this file");

    // Stale tmp files are removed
    assert!(!stale_chunk_tmp.exists());
    assert!(!stale_manifest_tmp.exists());
    assert!(!stale_unwritten_tmp.exists());

    // Manifest is fresh and valid
    let loaded = load_manifest(&bundle_dir).expect("fresh manifest must be valid");
    assert_eq!(loaded.original_file.name, "target.iso");
    assert_eq!(loaded.original_file.size_bytes, 14);
}

#[test]
fn test_startup_manifest_removal_propagates_non_not_found_error() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("locked.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Create a directory named manifest.json inside bundle_dir.
    // std::fs::remove_file on a directory fails with an error other than NotFound.
    let manifest_dir = bundle_dir.join("manifest.json");
    std::fs::create_dir(&manifest_dir).unwrap();

    let mut reader = Cursor::new(b"payload");
    let res = split_with_reader_unstable(
        &mut reader,
        "locked.bin",
        7,
        1024,
        1024,
        temp.path(),
        None,
        |_| {},
    );

    match res {
        Err(ChunkerError::Io(err)) => {
            assert_ne!(
                err.kind(),
                std::io::ErrorKind::NotFound,
                "error must not be NotFound"
            );
        }
        other => panic!("expected ChunkerError::Io, got {other:?}"),
    }

    // Assert no chunk file and no .tmp file was written
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".chk"),
            "found chunk file {name} when startup failed"
        );
        assert!(
            !name.ends_with(".tmp"),
            "found temporary file {name} when startup failed"
        );
    }
}

#[test]
fn test_zero_byte_file_success() {
    let temp = common::make_temp_dir();
    let mut reader = Cursor::new(Vec::<u8>::new());
    let mut events: Vec<ProgressEvent> = Vec::new();

    let summary = split_with_reader_unstable(
        &mut reader,
        "empty.txt",
        0,
        1024,
        1024,
        temp.path(),
        None,
        |ev| events.push(ev),
    )
    .expect("zero-byte split should succeed");

    assert_eq!(summary.reused_chunks, 0);
    assert_eq!(summary.bundle_path, temp.path().join("empty.txt.chunks"));
    assert!(
        events.is_empty(),
        "zero-byte file must emit 0 progress events"
    );

    let manifest = load_manifest(&summary.bundle_path).expect("manifest must be valid");
    assert_eq!(manifest.original_file.name, "empty.txt");
    assert_eq!(manifest.original_file.size_bytes, 0);
    assert_eq!(
        manifest.original_file.blake3,
        blake3::hash(b"").to_hex().as_str()
    );
    assert_eq!(manifest.total_chunks, 0);
    assert!(manifest.chunks.is_empty());
    validate_manifest(&manifest).expect("manifest must pass validation");

    // Check no chunk files or temporary files exist
    let entries: Vec<_> = std::fs::read_dir(&summary.bundle_path)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["manifest.json"]);
}

#[test]
fn test_boundary_sweep() {
    let buffer_size = 8usize;
    let chunk_sizes = [7u64, 8, 9];

    for &chunk_size in &chunk_sizes {
        let lengths = [
            0u64,
            1,
            chunk_size - 1,
            chunk_size,
            chunk_size + 1,
            2 * chunk_size,
            2 * chunk_size + 1,
        ];

        for &len in &lengths {
            let temp = common::make_temp_dir();
            let source_bytes = common::seeded_bytes(len ^ chunk_size, len as usize);
            let mut reader = Cursor::new(source_bytes.clone());
            let mut events: Vec<ProgressEvent> = Vec::new();

            let summary = split_with_reader_unstable(
                &mut reader,
                "data.bin",
                len,
                chunk_size,
                buffer_size,
                temp.path(),
                None,
                |ev| events.push(ev),
            )
            .unwrap_or_else(|e| {
                panic!("boundary split failed for chunk_size={chunk_size}, len={len}: {e}")
            });

            assert_eq!(summary.reused_chunks, 0);
            let bundle_dir = summary.bundle_path;

            // Independently derive expectations using source.chunks(chunk_size)
            let expected_chunks: Vec<&[u8]> = if source_bytes.is_empty() {
                Vec::new()
            } else {
                source_bytes.chunks(chunk_size as usize).collect()
            };
            let expected_total_chunks = expected_chunks.len();
            let expected_master_hash = blake3::hash(&source_bytes).to_hex().to_string();

            // Verify manifest
            let manifest = load_manifest(&bundle_dir).unwrap_or_else(|e| {
                panic!("failed to load manifest for chunk_size={chunk_size}, len={len}: {e}")
            });
            assert_eq!(manifest.total_chunks, expected_total_chunks);
            assert_eq!(manifest.original_file.size_bytes, len);
            assert_eq!(manifest.original_file.blake3, expected_master_hash);
            validate_manifest(&manifest).unwrap();

            // Verify chunks on disk and compare bytes and hash directly
            for (i, &expected_slice) in expected_chunks.iter().enumerate() {
                let chunk_path = bundle_dir.join(format!("part-{:04}.chk", i));
                assert!(
                    chunk_path.exists(),
                    "missing chunk {i} for chunk_size={chunk_size}, len={len}"
                );
                let chunk_bytes = std::fs::read(&chunk_path).unwrap();
                assert_eq!(
                    chunk_bytes.as_slice(),
                    expected_slice,
                    "chunk {i} bytes mismatch for chunk_size={chunk_size}, len={len}"
                );
                let expected_hash = blake3::hash(expected_slice).to_hex().to_string();
                assert_eq!(
                    manifest.chunks[i].blake3, expected_hash,
                    "manifest chunk {i} hash mismatch"
                );
                assert_eq!(
                    blake3::hash(&chunk_bytes).to_hex().as_str(),
                    manifest.chunks[i].blake3,
                    "disk chunk {i} hash mismatch"
                );
            }

            // Assert number of .chk files on disk equals manifest count
            let disk_chk_count = std::fs::read_dir(&bundle_dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".chk"))
                .count();
            assert_eq!(
                disk_chk_count, manifest.total_chunks,
                "disk .chk file count mismatch for chunk_size={chunk_size}, len={len}"
            );

            // Verify no leftover .tmp files
            for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                assert!(
                    !name.ends_with(".tmp"),
                    "found temporary file {name} in bundle"
                );
            }

            // Verify progress events
            if len == 0 {
                assert!(events.is_empty());
            } else {
                assert!(!events.is_empty());
                for w in events.windows(2) {
                    assert!(
                        w[1].bytes_processed >= w[0].bytes_processed,
                        "progress counter decreased: {} -> {}",
                        w[0].bytes_processed,
                        w[1].bytes_processed
                    );
                }
                let last = events.last().unwrap();
                assert_eq!(last.bytes_processed, len);
                assert_eq!(last.current_chunk, expected_total_chunks - 1);
                assert_eq!(last.total_chunks, expected_total_chunks);
                for ev in &events {
                    assert!(ev.current_chunk < expected_total_chunks);
                    assert_eq!(ev.total_chunks, expected_total_chunks);
                }
            }
        }
    }
}

#[test]
fn test_read_spanning_several_chunks_with_large_buffer() {
    let temp = common::make_temp_dir();
    let buffer_size = 70usize;
    let chunk_size = 30u64;
    let total_bytes = 150u64; // Exactly 5 chunks of 30 bytes; buffer of 70 spans >2 chunks per read

    let data = common::seeded_bytes(42, total_bytes as usize);
    let mut reader = Cursor::new(data.clone());
    let mut events: Vec<ProgressEvent> = Vec::new();

    let summary = split_with_reader_unstable(
        &mut reader,
        "multi.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |ev| events.push(ev),
    )
    .expect("split spanning several chunks must succeed");

    assert_eq!(summary.reused_chunks, 0);
    let manifest = load_manifest(&summary.bundle_path).unwrap();
    assert_eq!(manifest.total_chunks, 5);

    // Verify exactly 5 chunks exist and no part-0005.chk
    for i in 0..5 {
        let chunk_path = summary.bundle_path.join(format!("part-{:04}.chk", i));
        assert!(chunk_path.is_file());
        assert_eq!(chunk_path.metadata().unwrap().len(), 30);
    }
    assert!(!summary.bundle_path.join("part-0005.chk").exists());

    // Verify progress events:
    // current_chunk is the chunk holding the last byte of each read,
    // bytes_processed never decreases, and the final event is correct.
    assert!(!events.is_empty());
    for w in events.windows(2) {
        assert!(
            w[1].bytes_processed >= w[0].bytes_processed,
            "bytes_processed decreased: {} -> {}",
            w[0].bytes_processed,
            w[1].bytes_processed
        );
    }
    for ev in &events {
        let expected_chunk = ((ev.bytes_processed - 1) / chunk_size) as usize;
        assert_eq!(
            ev.current_chunk, expected_chunk,
            "current_chunk {} does not match chunk holding last byte of read ({})",
            ev.current_chunk, expected_chunk
        );
        assert!(
            ev.current_chunk < ev.total_chunks,
            "current_chunk {} must be < total_chunks {}",
            ev.current_chunk,
            ev.total_chunks
        );
        assert_eq!(ev.total_chunks, 5);
        assert_eq!(ev.total_bytes, total_bytes);
    }
    let final_ev = events.last().unwrap();
    assert_eq!(final_ev.bytes_processed, total_bytes);
    assert_eq!(final_ev.current_chunk, 4);
    assert_eq!(final_ev.total_chunks, 5);
    assert_eq!(final_ev.total_bytes, total_bytes);
}

#[test]
fn test_progress_reporting_exact_boundaries() {
    let temp = common::make_temp_dir();
    let chunk_size = 100u64;
    let buffer_size = 50usize;
    let total_bytes = 300u64; // 3 chunks

    let data = common::seeded_bytes(999, total_bytes as usize);
    let mut reader = Cursor::new(data);
    let mut events: Vec<ProgressEvent> = Vec::new();

    split_with_reader_unstable(
        &mut reader,
        "boundary_progress.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |ev| events.push(ev),
    )
    .expect("split should succeed");

    assert_eq!(events.len(), 6); // 300 / 50 = 6 iterations
    for ev in &events {
        assert!(
            ev.current_chunk < 3,
            "current_chunk {} must be < total_chunks 3",
            ev.current_chunk
        );
    }
    // At byte 100 (iteration index 1), chunk 0 was just finished
    assert_eq!(events[1].bytes_processed, 100);
    assert_eq!(events[1].current_chunk, 0);

    // At byte 200 (iteration index 3), chunk 1 was just finished
    assert_eq!(events[3].bytes_processed, 200);
    assert_eq!(events[3].current_chunk, 1);

    // Final event finishes at 300 with index 2
    let final_ev = events.last().unwrap();
    assert_eq!(final_ev.bytes_processed, 300);
    assert_eq!(final_ev.current_chunk, 2);
    assert_eq!(final_ev.total_chunks, 3);
}

#[test]
fn test_real_buffer_boundary_case() {
    let temp = common::make_temp_dir();
    let chunk_size = 1_048_577u64; // 1 MiB + 1 byte
    let buffer_size = BUFFER_SIZE; // 1 MiB = 1_048_576 bytes
    let total_bytes = 2 * chunk_size + 512; // > 2 chunks

    let data = common::seeded_bytes(777, total_bytes as usize);
    let mut reader = Cursor::new(data.clone());

    let summary = split_with_reader_unstable(
        &mut reader,
        "large_boundary.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("real buffer boundary split should succeed");

    assert_eq!(summary.reused_chunks, 0);
    let manifest = load_manifest(&summary.bundle_path).unwrap();
    assert_eq!(manifest.total_chunks, 3);
    assert_eq!(manifest.chunks[0].size_bytes, 1_048_577);
    assert_eq!(manifest.chunks[1].size_bytes, 1_048_577);
    assert_eq!(manifest.chunks[2].size_bytes, 512);

    let expected_master_hash = blake3::hash(&data).to_hex().to_string();
    assert_eq!(manifest.original_file.blake3, expected_master_hash);
    validate_manifest(&manifest).unwrap();

    // Compare on-disk chunk bytes and per-chunk hashes
    let chunk0_bytes = std::fs::read(summary.bundle_path.join("part-0000.chk")).unwrap();
    assert_eq!(chunk0_bytes.as_slice(), &data[..1_048_577]);
    assert_eq!(
        manifest.chunks[0].blake3,
        blake3::hash(&chunk0_bytes).to_hex().as_str()
    );

    let chunk1_bytes = std::fs::read(summary.bundle_path.join("part-0001.chk")).unwrap();
    assert_eq!(chunk1_bytes.as_slice(), &data[1_048_577..2 * 1_048_577]);
    assert_eq!(
        manifest.chunks[1].blake3,
        blake3::hash(&chunk1_bytes).to_hex().as_str()
    );

    let chunk2_bytes = std::fs::read(summary.bundle_path.join("part-0002.chk")).unwrap();
    assert_eq!(chunk2_bytes.as_slice(), &data[2 * 1_048_577..]);
    assert_eq!(
        manifest.chunks[2].blake3,
        blake3::hash(&chunk2_bytes).to_hex().as_str()
    );
}

#[test]
fn test_resplit_larger_then_smaller_removes_surplus_orphan_chunks() {
    let temp = common::make_temp_dir();
    let chunk_size = 100u64;
    let buffer_size = 50usize;

    // First split: 300 bytes (3 chunks: part-0000, part-0001, part-0002)
    let data_large = common::seeded_bytes(1, 300);
    let mut reader_large = Cursor::new(data_large);
    let summary1 = split_with_reader_unstable(
        &mut reader_large,
        "resplit.bin",
        300,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .unwrap();

    let bundle_dir = summary1.bundle_path;
    assert!(bundle_dir.join("part-0000.chk").exists());
    assert!(bundle_dir.join("part-0001.chk").exists());
    assert!(bundle_dir.join("part-0002.chk").exists());

    // Second split: 50 bytes (1 chunk) under same name and output dir
    let data_small = common::seeded_bytes(2, 50);
    let mut reader_small = Cursor::new(data_small.clone());
    let summary2 = split_with_reader_unstable(
        &mut reader_small,
        "resplit.bin",
        50,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .unwrap();

    assert_eq!(summary2.bundle_path, bundle_dir);
    assert_eq!(summary2.reused_chunks, 0);

    // Surplus chunks must be deleted
    assert!(bundle_dir.join("part-0000.chk").exists());
    assert_eq!(
        std::fs::read(bundle_dir.join("part-0000.chk")).unwrap(),
        data_small
    );
    assert!(
        !bundle_dir.join("part-0001.chk").exists(),
        "part-0001.chk should have been removed as orphan"
    );
    assert!(
        !bundle_dir.join("part-0002.chk").exists(),
        "part-0002.chk should have been removed as orphan"
    );

    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(manifest.total_chunks, 1);
    assert_eq!(manifest.original_file.size_bytes, 50);
    validate_manifest(&manifest).unwrap();
}

#[test]
fn test_early_eof_returns_unexpected_eof_and_cleans_tmp() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let expected_size = 150u64; // Expects 3 chunks of 50 bytes

    // Source only yields 75 bytes (completes chunk 0, partially fills chunk 1, then ends)
    let data = common::seeded_bytes(101, 75);
    let mut reader = common::EarlyEofReader::new(data);

    let res = split_with_reader_unstable(
        &mut reader,
        "early_eof.bin",
        expected_size,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    );

    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        }
        other => panic!("expected ChunkerError::Io(UnexpectedEof), got {other:?}"),
    }

    let bundle_dir = temp.path().join("early_eof.bin.chunks");
    // Completed chunk 0 must remain
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert_eq!(
        bundle_dir.join("part-0000.chk").metadata().unwrap().len(),
        50
    );

    // Incomplete chunk 1 must not exist, and no active .tmp files must remain
    assert!(!bundle_dir.join("part-0001.chk").exists());
    assert!(!bundle_dir.join("part-0001.chk.tmp").exists());
    assert!(!bundle_dir.join("manifest.json").exists());

    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".tmp"),
            "found temporary file {name} after early EOF"
        );
    }
}

#[test]
fn test_growth_returns_invalid_data_and_crossing_read_not_written() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let expected_size = 80u64; // Chunk 0 is 50, Chunk 1 would be 30

    // Source file actually has 110 bytes (grew by 30 bytes)
    // First read: 50 bytes (chunk 0 is completed)
    // Second read: 50 bytes (crosses from 50 to 100 > expected_size 80)
    let data = common::seeded_bytes(202, 110);
    let mut reader = common::GrowingReader::new(data);

    let res = split_with_reader_unstable(
        &mut reader,
        "grown.bin",
        expected_size,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    );

    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::InvalidData);
        }
        other => panic!("expected ChunkerError::Io(InvalidData), got {other:?}"),
    }

    let bundle_dir = temp.path().join("grown.bin.chunks");
    // Completed chunk 0 must remain
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert_eq!(
        bundle_dir.join("part-0000.chk").metadata().unwrap().len(),
        50
    );

    // The crossing read must NOT have been written to disk
    assert!(!bundle_dir.join("part-0001.chk").exists());
    assert!(!bundle_dir.join("part-0001.chk.tmp").exists());
    assert!(!bundle_dir.join("manifest.json").exists());
}

#[test]
fn test_interrupted_reads_retry_and_succeed() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 200u64; // 4 chunks of 50 bytes

    let data = common::seeded_bytes(303, total_bytes as usize);
    let mut reader = common::InterruptingReader::new(data, 10);

    let summary = split_with_reader_unstable(
        &mut reader,
        "interrupted.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("split must retry on Interrupted and succeed");

    assert_eq!(reader.total_interrupts_injected, 10);
    assert_eq!(summary.reused_chunks, 0);

    let manifest = load_manifest(&summary.bundle_path).unwrap();
    assert_eq!(manifest.total_chunks, 4);
    validate_manifest(&manifest).unwrap();
}

#[test]
fn test_cancellation_a_callback_sets_token_after_chunk_finishes() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 150u64; // 3 chunks

    let data = common::seeded_bytes(404, total_bytes as usize);
    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_a.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.bytes_processed == 50 {
                // Chunk 0 just finished, request cancellation
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled, got {res:?}"
    );

    let bundle_dir = temp.path().join("cancel_a.bin.chunks");
    // Completed chunk 0 is retained
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert_eq!(
        bundle_dir.join("part-0000.chk").metadata().unwrap().len(),
        50
    );

    // Later chunks do not exist
    assert!(!bundle_dir.join("part-0001.chk").exists());
    assert!(!bundle_dir.join("part-0002.chk").exists());
    assert!(!bundle_dir.join("manifest.json").exists());

    // No .tmp files left behind
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".tmp"),
            "found temporary file {name} after cancellation"
        );
    }
}

#[test]
fn test_cancellation_mid_chunk_cleans_partial_tmp_and_preserves_earlier_chunks() {
    let temp = common::make_temp_dir();
    let chunk_size = 100u64;
    let buffer_size = 25usize; // Smaller than chunk size
    let total_bytes = 250u64; // Chunk 0 (100), Chunk 1 (100), Chunk 2 (50)

    let data = common::seeded_bytes(404, total_bytes as usize);
    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_mid.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            // Cancel when bytes_processed is 125: chunk 0 is finished (100 bytes),
            // and chunk 1 is in progress with 25 bytes in part-0001.chk.tmp.
            if ev.bytes_processed == 125 {
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled on mid-chunk progress, got {res:?}"
    );

    let bundle_dir = temp.path().join("cancel_mid.bin.chunks");

    // Earlier finished chunk 0 is kept
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert_eq!(
        bundle_dir.join("part-0000.chk").metadata().unwrap().len(),
        100
    );

    // Partial chunk 1 was not renamed
    assert!(!bundle_dir.join("part-0001.chk").exists());
    assert!(!bundle_dir.join("part-0002.chk").exists());

    // No manifest was written
    assert!(!bundle_dir.join("manifest.json").exists());

    // Partial part-0001.chk.tmp must have been cleaned up by TmpFileGuard
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".tmp"),
            "found temporary file {name} after mid-chunk cancellation"
        );
    }
}

#[test]
fn test_cancellation_b_callback_sets_token_on_final_progress_event() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64; // 2 chunks

    let data = common::seeded_bytes(505, total_bytes as usize);
    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_b.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.bytes_processed == 100 {
                // Final progress event, set cancellation token
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled on final progress callback, got {res:?}"
    );

    let bundle_dir = temp.path().join("cancel_b.bin.chunks");
    // Both completed chunks are retained
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert_eq!(
        bundle_dir.join("part-0000.chk").metadata().unwrap().len(),
        50
    );
    assert!(bundle_dir.join("part-0001.chk").is_file());
    assert_eq!(
        bundle_dir.join("part-0001.chk").metadata().unwrap().len(),
        50
    );

    // No manifest was written!
    assert!(!bundle_dir.join("manifest.json").exists());

    // No .tmp files
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !name.ends_with(".tmp"),
            "found temporary file {name} after cancellation"
        );
    }
}

#[test]
fn test_cancellation_c_token_set_after_return_ok() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64;

    let data = common::seeded_bytes(606, total_bytes as usize);
    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(false));

    let summary = split_with_reader_unstable(
        &mut reader,
        "cancel_c.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        |_| {},
    )
    .expect("split should complete with Ok");

    // Token set AFTER split returns Ok
    token.store(true, Ordering::Relaxed);

    // Bundle and manifest remain intact and valid
    assert!(summary.bundle_path.join("part-0000.chk").is_file());
    assert!(summary.bundle_path.join("part-0001.chk").is_file());
    assert!(summary.bundle_path.join("manifest.json").is_file());

    let manifest = load_manifest(&summary.bundle_path).expect("manifest must be valid");
    assert_eq!(manifest.total_chunks, 2);
    validate_manifest(&manifest).expect("manifest must pass validation");
}

#[test]
fn test_precancelled_zero_byte_input() {
    let temp = common::make_temp_dir();
    let mut reader = Cursor::new(Vec::<u8>::new());
    let token = Arc::new(AtomicBool::new(true)); // Already set

    let res = split_with_reader_unstable(
        &mut reader,
        "precancelled_empty.txt",
        0,
        1024,
        1024,
        temp.path(),
        Some(&token),
        |_| {},
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled on pre-cancelled zero-byte input, got {res:?}"
    );

    let bundle_dir = temp.path().join("precancelled_empty.txt.chunks");
    assert!(!bundle_dir.join("manifest.json").exists());
}

struct ProbeCancellingReader {
    token: Arc<AtomicBool>,
}

impl Read for ProbeCancellingReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        self.token.store(true, Ordering::Relaxed);
        Ok(0)
    }
}

impl Seek for ProbeCancellingReader {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0)
    }
}

#[test]
fn test_zero_byte_cancellation_during_growth_probe_preserves_surplus_chunk() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("zero_probe_cancel.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Pre-create a surplus chunk that would be deleted if clean_orphan_chunks ran
    let surplus = bundle_dir.join("part-0099.chk");
    std::fs::write(&surplus, b"surplus orphan").unwrap();

    let token = Arc::new(AtomicBool::new(false));
    let mut reader = ProbeCancellingReader {
        token: token.clone(),
    };

    let res = split_with_reader_unstable(
        &mut reader,
        "zero_probe_cancel.bin",
        0,
        1024,
        1024,
        temp.path(),
        Some(&token),
        |_| {},
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled on zero-byte probe cancellation, got {res:?}"
    );

    // Surplus chunk must survive: cancellation happened before clean_orphan_chunks
    assert!(
        surplus.is_file(),
        "surplus part-0099.chk must survive when cancelled during zero-byte growth probe"
    );

    // No manifest was written
    assert!(!bundle_dir.join("manifest.json").exists());
}

#[test]
fn test_zero_byte_cancellation_pre_save_check_after_orphan_cleanup() {
    let temp = common::make_temp_dir();
    let bundle_dir = temp.path().join("zero_pre_save.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Create 100 surplus orphan chunks that clean_orphan_chunks(&bundle_dir, 0) will delete
    for i in 100..200 {
        let p = bundle_dir.join(format!("part-{:04}.chk", i));
        std::fs::write(&p, b"orphan").unwrap();
    }

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();
    let bundle_dir_clone = bundle_dir.clone();

    let watcher = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > std::time::Duration::from_secs(5) {
                return;
            }
            let count = std::fs::read_dir(&bundle_dir_clone)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| {
                            e.file_name()
                                .to_str()
                                .is_some_and(|n| n.starts_with("part-01"))
                        })
                        .count()
                })
                .unwrap_or(100);

            if count < 100 {
                // clean_orphan_chunks(&bundle_dir, 0) has started removing files!
                // Set cancellation token before save_manifest_atomic runs.
                token_clone.store(true, Ordering::SeqCst);
                return;
            }
            std::thread::yield_now();
        }
    });

    let mut reader = Cursor::new(Vec::<u8>::new());
    let res = split_with_reader_unstable(
        &mut reader,
        "zero_pre_save.bin",
        0,
        1024,
        1024,
        temp.path(),
        Some(&token),
        |_| {},
    );

    let _ = watcher.join();

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled from zero-byte pre-save check, got {res:?}"
    );

    assert!(
        !bundle_dir.join("manifest.json").exists(),
        "manifest.json must not exist when cancelled before zero-byte manifest save"
    );
}

// ── Fix 1: Growth detection ───────────────────────────────────────────────────

/// (a) Growth aligned to the buffer boundary: expected 16, buffer 8, source 24.
/// The read loop exits cleanly after reading 16 bytes (2 buffer-fills); the
/// probe must detect the remaining 8 bytes and return InvalidData.
#[test]
fn test_growth_aligned_to_buffer_boundary() {
    let temp = common::make_temp_dir();
    let expected_size: u64 = 16;
    let buffer_size: usize = 8;
    let chunk_size: u64 = 16; // one chunk covering all expected bytes

    // Source is 24 bytes: 8 bytes beyond expected_size, aligned to buffer_size
    let data = common::seeded_bytes(901, 24);
    let mut reader = common::GrowingReader::new(data);

    let res = split_with_reader_unstable(
        &mut reader,
        "aligned_growth.bin",
        expected_size,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    );

    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::InvalidData, "must be InvalidData");
            assert!(
                err.to_string().contains("grew"),
                "error message must mention growth: {err}"
            );
        }
        other => panic!("expected InvalidData growth error, got {other:?}"),
    }

    // No manifest must exist
    let bundle_dir = temp.path().join("aligned_growth.bin.chunks");
    assert!(!bundle_dir.join("manifest.json").exists());
}

/// (b) Expected 0 but source has bytes: the zero-byte path must probe and
/// detect growth.
#[test]
fn test_growth_zero_expected_non_empty_source() {
    let temp = common::make_temp_dir();
    // Source has 5 bytes but we claim expected_size=0
    let data = common::seeded_bytes(902, 5);
    let mut reader = common::GrowingReader::new(data);

    let res = split_with_reader_unstable(
        &mut reader,
        "zero_growth.bin",
        0,
        1024,
        1024,
        temp.path(),
        None,
        |_| {},
    );

    match res {
        Err(ChunkerError::Io(err)) => {
            assert_eq!(err.kind(), ErrorKind::InvalidData, "must be InvalidData");
            assert!(
                err.to_string().contains("grew"),
                "error message must mention growth: {err}"
            );
        }
        other => panic!("expected InvalidData for zero-expected non-empty source, got {other:?}"),
    }

    let bundle_dir = temp.path().join("zero_growth.bin.chunks");
    assert!(!bundle_dir.join("manifest.json").exists());
}

/// (c) Source is exactly expected_size: probe returns 0, split succeeds.
#[test]
fn test_no_false_growth_when_source_is_exact_size() {
    let temp = common::make_temp_dir();
    let expected_size: u64 = 16;
    let buffer_size: usize = 8;
    let chunk_size: u64 = 16;

    let data = common::seeded_bytes(903, expected_size as usize);
    let mut reader = Cursor::new(data);

    let summary = split_with_reader_unstable(
        &mut reader,
        "exact_size.bin",
        expected_size,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("exact-size source must succeed; probe must return 0");

    let bundle_dir = summary.bundle_path;
    assert!(bundle_dir.join("manifest.json").exists());
    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(manifest.total_chunks, 1);
    assert_eq!(manifest.original_file.size_bytes, expected_size);
    validate_manifest(&manifest).unwrap();
}

// ── Fix 2: Cancellation before orphan cleanup ─────────────────────────────────

/// Cancel from the FINAL progress callback (all chunks written, no manifest yet).
/// A surplus `part-0099.chk` must survive (orphan cleanup was not reached).
/// All written chunks must exist. No manifest. No .tmp files.
#[test]
fn test_cancellation_before_orphan_cleanup_preserves_surplus_chunk() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64; // 2 chunks

    let data = common::seeded_bytes(1001, total_bytes as usize);
    let bundle_dir = temp.path().join("cancel_preorphan.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    let surplus = bundle_dir.join("part-0099.chk");

    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();
    let bundle_dir_clone = bundle_dir.clone();

    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_preorphan.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.bytes_processed == total_bytes {
                // Create surplus orphan chunk from inside final progress callback (after early cleanup)
                let surplus = bundle_dir_clone.join("part-0099.chk");
                std::fs::write(&surplus, b"surplus data").unwrap();
                // Final callback — set after all chunks written, before end-of-split orphan cleanup
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled, got {res:?}"
    );

    // Both written chunks retained
    assert!(
        bundle_dir.join("part-0000.chk").is_file(),
        "part-0000 must exist"
    );
    assert!(
        bundle_dir.join("part-0001.chk").is_file(),
        "part-0001 must exist"
    );

    // Surplus chunk must NOT have been cleaned (orphan cleanup not reached)
    assert!(
        surplus.exists(),
        "surplus part-0099.chk must survive when cancelled before orphan cleanup"
    );

    // No manifest, no .tmp files
    assert!(
        !bundle_dir.join("manifest.json").exists(),
        "must have no manifest"
    );
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(!name.ends_with(".tmp"), "found .tmp file: {name}");
    }
}

// ── Entry cancellation check coverage ────────────────────────────────────────

/// Token is set before calling split on a non-empty source; must return
/// Cancelled immediately at entry without reading or writing anything.
#[test]
fn test_precancelled_non_empty_input_at_entry() {
    // The entry check fires BEFORE any side effects (create_dir_all, remove_file).
    // We prove this by pre-creating the bundle dir with a stale manifest.json and
    // asserting the stale manifest is still there after the call — which can only
    // be true if the entry check ran before the remove_file side-effect.
    let temp = common::make_temp_dir();

    // Pre-create the bundle directory and place a stale manifest inside it.
    let bundle_dir = temp.path().join("pre_cancel_entry.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();
    let stale_manifest = bundle_dir.join("manifest.json");
    std::fs::write(&stale_manifest, b"stale").unwrap();

    let data = common::seeded_bytes(5555, 100);
    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(true)); // already set at entry

    let res = split_with_reader_unstable(
        &mut reader,
        "pre_cancel_entry.bin",
        100,
        50,
        50,
        temp.path(),
        Some(&token),
        |_| {},
    );

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled at entry, got {res:?}"
    );

    // The stale manifest must still exist: the entry check must have returned
    // Cancelled before the remove_file(old_manifest) side-effect ran.
    assert!(
        stale_manifest.exists(),
        "stale manifest was removed — entry check did not fire before remove_file"
    );
}

#[test]
fn test_cancellation_pre_save_check_after_orphan_cleanup() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64; // 2 chunks: part-0000.chk, part-0001.chk

    let data = common::seeded_bytes(777, total_bytes as usize);
    let mut reader = Cursor::new(data);
    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();
    let created_flag = Arc::new(AtomicBool::new(false));
    let created_flag_clone = created_flag.clone();

    let bundle_dir = temp.path().join("cancel_pre_save.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Watcher thread: wait until surplus chunks are created by final progress callback,
    // then wait until clean_orphan_chunks starts deleting them (count < 100),
    // then set the cancellation token before manifest save.
    let bundle_dir_clone = bundle_dir.clone();
    let watcher = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > std::time::Duration::from_secs(5) {
                return;
            }
            if !created_flag_clone.load(Ordering::SeqCst) {
                std::thread::yield_now();
                continue;
            }
            let count = std::fs::read_dir(&bundle_dir_clone)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| {
                            e.file_name()
                                .to_str()
                                .is_some_and(|n| n.starts_with("part-01"))
                        })
                        .count()
                })
                .unwrap_or(100);

            if count < 100 {
                // clean_orphan_chunks has started removing files!
                // Set cancellation token for the pre-save check.
                token_clone.store(true, Ordering::SeqCst);
                return;
            }
            std::thread::yield_now();
        }
    });

    let bundle_dir_for_progress = bundle_dir.clone();
    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_pre_save.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.bytes_processed == total_bytes {
                // Create 100 surplus orphan chunks from inside final progress callback
                for i in 100..200 {
                    let p = bundle_dir_for_progress.join(format!("part-{:04}.chk", i));
                    std::fs::write(&p, b"orphan").unwrap();
                }
                created_flag.store(true, Ordering::SeqCst);
            }
        },
    );

    let _ = watcher.join();

    assert!(
        matches!(res, Err(ChunkerError::Cancelled)),
        "expected Cancelled from pre-save check, got {res:?}"
    );

    // Both valid chunks were created
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert!(bundle_dir.join("part-0001.chk").is_file());

    // No manifest must be written
    assert!(
        !bundle_dir.join("manifest.json").exists(),
        "manifest.json must not exist when cancelled before save"
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
fn test_split_chunk_temp_is_directory() {
    let temp = common::make_temp_dir();

    // Scratch layout probe to determine platform error kind for create_new on directory
    let scratch_dir = temp.path().join("scratch_dir");
    std::fs::create_dir_all(&scratch_dir).unwrap();
    let scratch_res = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch_dir);
    let scratch_err = scratch_res.expect_err("create_new on directory must fail");
    let scratch_kind = scratch_err.kind();
    let scratch_raw = scratch_err.raw_os_error();

    let bundle_dir = temp.path().join("dir_tmp.bin.chunks");
    std::fs::create_dir_all(&bundle_dir).unwrap();
    let tmp_path = bundle_dir.join("part-0000.chk.tmp");
    std::fs::create_dir_all(&tmp_path).unwrap();
    let marker_file = tmp_path.join("marker.txt");
    let marker_bytes = b"marker content inside chunk temp directory".to_vec();
    std::fs::write(&marker_file, &marker_bytes).unwrap();

    let data = vec![0x42u8; 100];
    let mut reader = Cursor::new(data);
    let mut progress_count = 0;
    let res = split_with_reader_unstable(
        &mut reader,
        "dir_tmp.bin",
        100,
        100,
        100,
        temp.path(),
        None,
        |_| progress_count += 1,
    );

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
    assert_eq!(read_file_content(&marker_file), marker_bytes);
    assert!(!bundle_dir.join("part-0000.chk").exists());
    assert_eq!(progress_count, 0);
}
