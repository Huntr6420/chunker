mod common;

use std::io::{Cursor, ErrorKind, Seek};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chunker_core::error::ChunkerError;
use chunker_core::manifest::{load_manifest, validate_manifest};
use chunker_core::progress::ProgressEvent;
use chunker_core::split::split_with_reader_unstable;
use common::CancellingOnNthReadReader;

/// Helper: performs an initial split of `data` with `chunk_size` into `temp_dir/filename.chunks/`.
fn initial_split(
    filename: &str,
    data: &[u8],
    chunk_size: u64,
    buffer_size: usize,
    output_dir: &std::path::Path,
) -> std::path::PathBuf {
    let mut reader = Cursor::new(data.to_vec());
    let summary = split_with_reader_unstable(
        &mut reader,
        filename,
        data.len() as u64,
        chunk_size,
        buffer_size,
        output_dir,
        None,
        |_| {},
    )
    .expect("initial split must succeed");
    summary.bundle_path
}

// ── 1. Same-size corrupted middle chunk (also mutation matrix a) ─────────────

#[test]
fn test_resume_same_size_corrupted_middle_chunk() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 25usize;
    let total_bytes = 150u64; // 3 chunks: 0, 1, 2
    let data = common::seeded_bytes(101, total_bytes as usize);

    let bundle_dir = initial_split(
        "corrupt_mid.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Corrupt chunk 1 with same-size different bytes
    let chunk1_path = bundle_dir.join("part-0001.chk");
    let corrupt_data = common::seeded_bytes(999, chunk_size as usize);
    std::fs::write(&chunk1_path, &corrupt_data).unwrap();

    // Re-split using the same source
    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "corrupt_mid.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume should succeed");

    assert_eq!(summary.reused_chunks, 1);

    // Verify all 3 chunks on disk match source
    let mut reconstructed = Vec::new();
    for i in 0..3 {
        let p = bundle_dir.join(format!("part-{:04}.chk", i));
        let chunk_bytes = std::fs::read(&p).unwrap();
        assert_eq!(chunk_bytes.len() as u64, chunk_size);
        reconstructed.extend_from_slice(&chunk_bytes);
    }
    assert_eq!(reconstructed, data);

    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(manifest.total_chunks, 3);
    assert_eq!(
        manifest.original_file.blake3,
        blake3::hash(&data).to_hex().as_str()
    );
    validate_manifest(&manifest).unwrap();
}

// ── 2. Missing chunk ────────────────────────────────────────────────────────

#[test]
fn test_resume_missing_chunk() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 150u64; // 3 chunks
    let data = common::seeded_bytes(102, total_bytes as usize);

    let bundle_dir = initial_split("missing.bin", &data, chunk_size, buffer_size, temp.path());

    // Delete chunk 1
    std::fs::remove_file(bundle_dir.join("part-0001.chk")).unwrap();

    // Re-split
    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "missing.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume should succeed");

    assert_eq!(summary.reused_chunks, 1);
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert!(bundle_dir.join("part-0001.chk").is_file());
    assert!(bundle_dir.join("part-0002.chk").is_file());

    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(manifest.total_chunks, 3);
    validate_manifest(&manifest).unwrap();
}

// ── 3. Truncated chunk ──────────────────────────────────────────────────────

#[test]
fn test_resume_truncated_chunk() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 150u64;
    let data = common::seeded_bytes(103, total_bytes as usize);

    let bundle_dir = initial_split("truncated.bin", &data, chunk_size, buffer_size, temp.path());

    // Truncate chunk 1 to 20 bytes
    std::fs::write(bundle_dir.join("part-0001.chk"), &data[50..70]).unwrap();

    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "truncated.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume should succeed");

    assert_eq!(summary.reused_chunks, 1);
    assert_eq!(
        std::fs::metadata(bundle_dir.join("part-0001.chk"))
            .unwrap()
            .len(),
        chunk_size
    );
}

// ── 4. All chunks valid without manifest ────────────────────────────────────

#[test]
fn test_resume_all_chunks_valid_without_manifest() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 150u64;
    let data = common::seeded_bytes(104, total_bytes as usize);

    let bundle_dir = initial_split("all_valid.bin", &data, chunk_size, buffer_size, temp.path());

    // Remove manifest
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "all_valid.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume should succeed");

    assert_eq!(summary.reused_chunks, 3);

    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(manifest.total_chunks, 3);
    assert_eq!(
        manifest.original_file.blake3,
        blake3::hash(&data).to_hex().as_str()
    );
    validate_manifest(&manifest).unwrap();
}

// ── 5. Changed source content with same length ──────────────────────────────

#[test]
fn test_resume_changed_source_content_same_length() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64;
    let data_a = common::seeded_bytes(201, total_bytes as usize);
    let data_b = common::seeded_bytes(202, total_bytes as usize);

    let bundle_dir = initial_split(
        "changed_source.bin",
        &data_a,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Re-split with data_b of same length
    let mut reader = Cursor::new(data_b.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "changed_source.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("split with new content should succeed");

    assert_eq!(summary.reused_chunks, 0);

    let chunk0 = std::fs::read(bundle_dir.join("part-0000.chk")).unwrap();
    assert_eq!(chunk0, &data_b[..50]);

    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(
        manifest.original_file.blake3,
        blake3::hash(&data_b).to_hex().as_str()
    );
}

// ── 6. Changed chunk size ───────────────────────────────────────────────────

#[test]
fn test_resume_changed_chunk_size() {
    let temp = common::make_temp_dir();
    let total_bytes = 150u64;
    let data = common::seeded_bytes(301, total_bytes as usize);

    // Initial split with chunk_size 50 -> 3 chunks
    let bundle_dir = initial_split("changed_chunk_size.bin", &data, 50, 50, temp.path());
    assert!(bundle_dir.join("part-0002.chk").is_file());

    // Re-split with chunk_size 75 -> 2 chunks
    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "changed_chunk_size.bin",
        total_bytes,
        75,
        50,
        temp.path(),
        None,
        |_| {},
    )
    .expect("re-split with new chunk size should succeed");

    assert_eq!(summary.reused_chunks, 0);
    assert_eq!(
        std::fs::metadata(bundle_dir.join("part-0000.chk"))
            .unwrap()
            .len(),
        75
    );
    assert_eq!(
        std::fs::metadata(bundle_dir.join("part-0001.chk"))
            .unwrap()
            .len(),
        75
    );
    // Surplus chunk 2 must be deleted
    assert!(!bundle_dir.join("part-0002.chk").exists());

    let manifest = load_manifest(&bundle_dir).unwrap();
    assert_eq!(manifest.total_chunks, 2);
    validate_manifest(&manifest).unwrap();
}

// ── 7. Cancellation at top of resume loop (mutation matrix f) ────────────────

#[test]
fn test_resume_cancellation_at_top_of_resume_loop() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64; // 2 chunks
    let data = common::seeded_bytes(401, total_bytes as usize);

    let bundle_dir = initial_split(
        "cancel_resume_top.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let mut reader = Cursor::new(data.clone());
    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_resume_top.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.current_chunk == 0 {
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(matches!(res, Err(ChunkerError::Cancelled)));
    // Reader must not have read into chunk 1
    assert_eq!(
        reader.stream_position().unwrap(),
        50,
        "reader must not read chunk 1 source bytes when cancelled at top of resume loop"
    );
    // All existing chunks are preserved
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert!(bundle_dir.join("part-0001.chk").is_file());
    assert!(!bundle_dir.join("manifest.json").exists());
}

// ── 8. Cancellation inside per-chunk verification loop (mutation matrix g) ───

#[test]
fn test_resume_cancellation_inside_verification_read_loop() {
    let temp = common::make_temp_dir();
    let chunk_size = 100u64;
    let buffer_size = 25usize; // smaller than chunk so verification does multiple reads
    let total_bytes = 100u64;
    let data = common::seeded_bytes(402, total_bytes as usize);

    let bundle_dir = initial_split(
        "cancel_mid_verify.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    let token = Arc::new(AtomicBool::new(false));
    // Set token on the 2nd read() call (at byte 25/100 of chunk 0 verification)
    let mut reader = CancellingOnNthReadReader::new(data, 2, token.clone());

    let mut events = Vec::new();
    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_mid_verify.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        |ev| events.push(ev),
    );

    assert!(matches!(res, Err(ChunkerError::Cancelled)));
    // Mid-chunk cancellation must abort before candidate 0 is verified, so no progress event is emitted
    assert!(
        events.is_empty(),
        "no progress events must be emitted when cancelled mid-verification of chunk 0"
    );
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert!(!bundle_dir.join("manifest.json").exists());
}

// ── 9. Stale manifest removal verified when re-split is cancelled ────────────

#[test]
fn test_resume_stale_manifest_deleted_on_cancelled_resplit() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64;
    let data = common::seeded_bytes(403, total_bytes as usize);

    let bundle_dir = initial_split(
        "stale_manifest.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Overwrite manifest with stale data
    let manifest_path = bundle_dir.join("manifest.json");
    std::fs::write(&manifest_path, b"stale data").unwrap();

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let mut reader = Cursor::new(data.clone());
    let res = split_with_reader_unstable(
        &mut reader,
        "stale_manifest.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.current_chunk == 0 {
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(matches!(res, Err(ChunkerError::Cancelled)));
    assert!(
        !manifest_path.exists(),
        "stale manifest must be deleted and not replaced"
    );
}

// ── 10. Source shrank after first split (Addition B) ─────────────────────────

#[test]
fn test_resume_source_shrank_after_first_split() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 25usize;
    let original_total_bytes = 100u64; // 2 chunks: chunk 0 (50), chunk 1 (50)
    let full_data = common::seeded_bytes(501, original_total_bytes as usize);

    let bundle_dir = initial_split(
        "shrunk.bin",
        &full_data,
        chunk_size,
        buffer_size,
        temp.path(),
    );
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    // Source shrank: reader only provides 75 bytes (chunk 0 is 50 bytes, chunk 1 is truncated to 25)
    // but expected_size is kept at 100.
    let truncated_data = full_data[..75].to_vec();
    let mut reader = Cursor::new(truncated_data);

    let res = split_with_reader_unstable(
        &mut reader,
        "shrunk.bin",
        original_total_bytes, // keep expected_size at original value
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
        other => panic!("expected UnexpectedEof, got {other:?}"),
    }

    // Chunk 0 was verified and kept
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert!(!bundle_dir.join("manifest.json").exists());

    // No .tmp left behind
    for entry in std::fs::read_dir(&bundle_dir).unwrap().flatten() {
        assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
    }
}

// ── 11. Master hasher restore after failed candidate (mutation matrix b) ─────

#[test]
fn test_resume_master_hasher_restored_after_failed_candidate() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 25usize;
    let total_bytes = 150u64; // 3 chunks
    let data = common::seeded_bytes(601, total_bytes as usize);

    let bundle_dir = initial_split(
        "hasher_restore.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Corrupt chunk 1 in the SECOND buffer (buffer_size is 25) so the first buffer
    // is fed into master_hasher before the mismatch is detected.
    let mut corrupt_chunk1 = data[50..100].to_vec();
    corrupt_chunk1[30] ^= 0xFF;
    std::fs::write(bundle_dir.join("part-0001.chk"), &corrupt_chunk1).unwrap();

    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "hasher_restore.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume must succeed");

    assert_eq!(summary.reused_chunks, 1);

    // The master hash in manifest MUST match blake3 of the whole source.
    // If the master hasher was NOT restored after candidate 1 failed, it would contain
    // duplicate bytes from the failed candidate 1 pass.
    let manifest = load_manifest(&bundle_dir).unwrap();
    let expected_master = blake3::hash(&data).to_hex().to_string();
    assert_eq!(
        manifest.original_file.blake3, expected_master,
        "master hash mismatch indicates master hasher was not rolled back"
    );
}

// ── 12. Reader position restore after failed candidate (mutation matrix c) ───

#[test]
fn test_resume_reader_position_restored_after_failed_candidate() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 25usize;
    let total_bytes = 150u64; // 3 chunks
    let data = common::seeded_bytes(701, total_bytes as usize);

    let bundle_dir = initial_split(
        "pos_restore.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Corrupt chunk 1 in the SECOND buffer (buffer_size is 25) so the first buffer
    // is fed into master_hasher before the mismatch is detected.
    let mut corrupt_chunk1 = data[50..100].to_vec();
    corrupt_chunk1[30] ^= 0xFF;
    std::fs::write(bundle_dir.join("part-0001.chk"), &corrupt_chunk1).unwrap();

    let mut reader = Cursor::new(data.clone());
    let summary = split_with_reader_unstable(
        &mut reader,
        "pos_restore.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume must succeed");

    assert_eq!(summary.reused_chunks, 1);

    // If reader position was not restored, chunk 1 written to disk would contain
    // bytes from position 100 instead of position 50!
    let chunk1_bytes = std::fs::read(bundle_dir.join("part-0001.chk")).unwrap();
    assert_eq!(
        chunk1_bytes,
        &data[50..100],
        "chunk 1 bytes mismatch indicates reader position was not restored"
    );
}

// ── 13. Early cleanup from first invalid index (mutation matrix d) ───────────

#[test]
fn test_resume_early_cleanup_removes_old_surplus_after_mismatch() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 150u64; // 3 chunks
    let data = common::seeded_bytes(801, total_bytes as usize);

    let bundle_dir = initial_split(
        "early_clean.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Corrupt chunk 1. Chunk 2 remains on disk (an old valid-looking chunk).
    std::fs::write(
        bundle_dir.join("part-0001.chk"),
        common::seeded_bytes(999, 50),
    )
    .unwrap();

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let mut reader = Cursor::new(data.clone());
    // Cancel in write phase right after chunk 1 is written
    let res = split_with_reader_unstable(
        &mut reader,
        "early_clean.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            // Chunk 0 was reused. Chunk 1 is written at bytes_processed = 100.
            if ev.bytes_processed == 100 {
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(matches!(res, Err(ChunkerError::Cancelled)));

    // Early cleanup ran from index 1: chunk 1 was rewritten, but chunk 2 MUST BE ABSENT
    // because early cleanup deleted it before writing started.
    assert!(bundle_dir.join("part-0000.chk").is_file());
    assert!(bundle_dir.join("part-0001.chk").is_file());
    assert!(
        !bundle_dir.join("part-0002.chk").exists(),
        "part-0002.chk must be removed by early cleanup before writing starts"
    );
}

// ── 14. Cancellation immediately before early cleanup (mutation matrix e) ────

#[test]
fn test_resume_cancellation_immediately_before_early_cleanup() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64; // 2 chunks
    let data = common::seeded_bytes(901, total_bytes as usize);

    let bundle_dir = initial_split(
        "cancel_pre_early_clean.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Pre-create surplus chunk 2
    let surplus = bundle_dir.join("part-0002.chk");
    std::fs::write(&surplus, b"surplus").unwrap();
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    let token = Arc::new(AtomicBool::new(false));
    let token_clone = token.clone();

    let mut reader = Cursor::new(data.clone());
    // All chunks valid; token set from the LAST verified chunk's callback (chunk 1)
    let res = split_with_reader_unstable(
        &mut reader,
        "cancel_pre_early_clean.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        Some(&token),
        move |ev| {
            if ev.current_chunk == 1 {
                token_clone.store(true, Ordering::Relaxed);
            }
        },
    );

    assert!(matches!(res, Err(ChunkerError::Cancelled)));
    // Surplus chunk must survive: cancellation check immediately before early cleanup fired
    assert!(
        surplus.is_file(),
        "surplus chunk must survive when cancelled before early cleanup"
    );
    assert!(!bundle_dir.join("manifest.json").exists());
}

// ── 15. Progress semantics and rollback ─────────────────────────────────────

#[test]
fn test_resume_progress_semantics_and_rollback() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 25usize;
    let total_bytes = 150u64; // 3 chunks
    let data = common::seeded_bytes(1001, total_bytes as usize);

    let bundle_dir = initial_split(
        "progress_rollback.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );

    // Corrupt chunk 1 in the SECOND buffer (buffer_size is 25) so the first buffer
    // is fed into master_hasher before the mismatch is detected.
    let mut corrupt_chunk1 = data[50..100].to_vec();
    corrupt_chunk1[30] ^= 0xFF;
    std::fs::write(bundle_dir.join("part-0001.chk"), &corrupt_chunk1).unwrap();

    let mut events = Vec::new();
    let mut reader = Cursor::new(data);
    let summary = split_with_reader_unstable(
        &mut reader,
        "progress_rollback.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |ev| events.push(ev),
    )
    .expect("resume should succeed");

    assert_eq!(summary.reused_chunks, 1);

    // Progress events:
    // 1. One event emitted for verified candidate 0 (bytes_processed = 50, current_chunk = 0)
    // 2. Candidate 1 failed verification -> NO event emitted during verification!
    // 3. Write phase for chunk 1 and chunk 2 emits events advancing monotonically
    assert!(!events.is_empty());
    for w in events.windows(2) {
        assert!(
            w[1].bytes_processed >= w[0].bytes_processed,
            "bytes_processed must never decrease: {} -> {}",
            w[0].bytes_processed,
            w[1].bytes_processed
        );
    }

    let first_ev = &events[0];
    assert_eq!(first_ev.current_chunk, 0);
    assert_eq!(first_ev.bytes_processed, 50);

    let final_ev = events.last().unwrap();
    assert_eq!(final_ev.bytes_processed, 150);
    assert_eq!(final_ev.current_chunk, 2);
    assert_eq!(final_ev.total_chunks, 3);
}

// ── 16. Short reads from source during resume verification ───────────────────

#[test]
fn test_resume_with_short_reads_from_source() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 25usize;
    let total_bytes = 100u64; // 2 chunks
    let data = common::seeded_bytes(1101, total_bytes as usize);

    // Initial split to create a fully valid bundle
    let bundle_dir = initial_split(
        "short_read.bin",
        &data,
        chunk_size,
        buffer_size,
        temp.path(),
    );
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    // Source reader returns at most 3 bytes per read() call
    let mut reader = common::ShortReadReader::new(data, 3);

    let summary = split_with_reader_unstable(
        &mut reader,
        "short_read.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |_| {},
    )
    .expect("resume with short reads should succeed");

    assert_eq!(
        summary.reused_chunks, 2,
        "expected all 2 chunks to be reused despite short reads from source"
    );
}

#[test]
fn test_resume_source_ends_early_inside_chunk() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let original_total_bytes = 100u64;
    let full_data = common::seeded_bytes(888, original_total_bytes as usize);

    let bundle_dir = initial_split(
        "early_eof.bin",
        &full_data,
        chunk_size,
        buffer_size,
        temp.path(),
    );
    std::fs::remove_file(bundle_dir.join("manifest.json")).unwrap();

    // Source only provides 25 bytes (ends early inside chunk 0, which is 50 bytes)
    // while expected_size is 100.
    let truncated_data = full_data[..25].to_vec();
    let mut reader = Cursor::new(truncated_data);

    let res = split_with_reader_unstable(
        &mut reader,
        "early_eof.bin",
        original_total_bytes,
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
        other => panic!("expected UnexpectedEof, got {other:?}"),
    }
}

fn read_file_content(path: &std::path::Path) -> Vec<u8> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).unwrap();
    buf
}

#[test]
fn test_resume_chunk_temp_occupied_after_cleanup() {
    let temp = common::make_temp_dir();
    let chunk_size = 50u64;
    let buffer_size = 50usize;
    let total_bytes = 100u64; // 2 chunks
    let data = common::seeded_bytes(201, total_bytes as usize);

    let bundle_dir = initial_split("occupied.bin", &data, chunk_size, buffer_size, temp.path());

    // Delete chunk 1 so chunk 0 is reusable, and chunk 1 is missing
    std::fs::remove_file(bundle_dir.join("part-0001.chk")).unwrap();

    let tmp_chunk_path = bundle_dir.join("part-0001.chk.tmp");
    let occupied_bytes = b"pre-existing occupied chunk tmp content that must survive".to_vec();

    let mut progress_events = Vec::new();
    let tmp_path_for_cb = tmp_chunk_path.clone();
    let occupied_bytes_for_cb = occupied_bytes.clone();

    let mut reader = Cursor::new(data);
    let res = split_with_reader_unstable(
        &mut reader,
        "occupied.bin",
        total_bytes,
        chunk_size,
        buffer_size,
        temp.path(),
        None,
        |event: ProgressEvent| {
            if event.current_chunk == 0 {
                // Reusable chunk 0 progress event in resume pass.
                // Startup cleanup has already completed.
                // Create the occupied tmp file at part-0001.chk.tmp now.
                std::fs::write(&tmp_path_for_cb, &occupied_bytes_for_cb).unwrap();
            }
            progress_events.push(event);
        },
    );

    match res {
        Err(ref err @ ChunkerError::Io(ref io_err)) => {
            assert_eq!(io_err.kind(), std::io::ErrorKind::AlreadyExists);
            let expected_msg = format!(
                "I/O error: Temporary file already exists: {}. If it is a leftover from an earlier run, remove it and retry.",
                tmp_chunk_path.display()
            );
            assert_eq!(err.to_string(), expected_msg);
        }
        other => panic!("expected Io(AlreadyExists), got {other:?}"),
    }

    assert!(tmp_chunk_path.is_file());
    assert_eq!(read_file_content(&tmp_chunk_path), occupied_bytes);
    assert!(!bundle_dir.join("part-0001.chk").exists());

    // Exactly 1 progress event from the resume pass (chunk 0), zero write-phase progress events
    assert_eq!(progress_events.len(), 1);
    assert_eq!(progress_events[0].current_chunk, 0);
    assert_eq!(progress_events[0].bytes_processed, chunk_size);
}
