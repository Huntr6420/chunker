# Chunker Implementation Plan

**Goal:** Build a memory-bounded Rust file splitter, verifier and merger for large files. Follow [design revision 2](../specs/2026-11-16-chunker-design.md) and `AGENTS.md`.

**Workflow:** Complete one task at a time. Write a failing test, observe the expected failure, implement, and rerun it. Finish each task with `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`. Explain the change and stop for review. The owner commits. No unapproved dependencies or destructive commands.

**Constraints:** Rust 2024; toolchain 1.85.0 and tracked `Cargo.lock`; fixed 1 MiB I/O buffer plus manifest metadata growing with chunk count; public split and CLI minimum chunk size 1 MiB, manifest requires a positive chunk size; BLAKE3 hashes; append `.tmp`, `sync_all`, close and rename. Windows, Linux and macOS; no compression, encryption, parallel I/O or GUI. Core has no terminal output. Cancellation uses `Option<&Arc<AtomicBool>>` and progress `FnMut(ProgressEvent)`.

**Progress:** `current_chunk` is the zero-based index of the chunk containing the last byte of the read that generated the event. CLI displays index + 1; empty files have zero chunks and never display 1/0.

**Tests:** Use deterministic seeded non-periodic data, `tempfile::tempdir()` with its guard retained, and small fixtures. For a real buffer-boundary test, use 1,048,577-byte chunks and more than two chunks of data. Compare reconstructed bytes and hashes.

## Task 1: Workspace and dependencies

Files: workspace and package manifests, `rust-toolchain.toml`, `Cargo.lock`, `core/src/lib.rs`, `cli/src/main.rs`, `core/tests/env_check_test.rs`.

- [ ] Test 1 MiB `BUFFER_SIZE`, 500 MiB `DEFAULT_CHUNK_SIZE`, and `fs4::available_space`.
- [ ] Set resolver `3`, both package `rust-version = "1.85"`, library package `chunker-core`, and CLI binary `chunker`. Use `CARGO_BIN_EXE_chunker` in CLI tests. Add only approved dependencies, including `tempfile = "3"` as a dev-dependency to both packages.
- [ ] Pin `channel = "1.85.0"` and `components = ["rustfmt", "clippy"]`; keep `Cargo.lock` tracked for the owner. Verify toolchain compatibility and quality gate; stop for review.

## Task 2: Errors, progress, safety and cleanup

Files: `core/src/error.rs`, `progress.rs`, `guard.rs`, `security.rs`, `fs_utils.rs`, `lib.rs`, and `core/tests/helpers_and_guard_test.rs`.

- [ ] Test errors, temporary guards on drop/unwind and disarm, scoped stale/orphan cleanup, and safe parent fallback.
- [ ] Extract split input final components using both separator styles; reject unsafe and reserved names. Manifest names must already be plain safe filenames. Test traversal, invalid characters, reserved prefixes, trailing dots/spaces and both separators.
- [ ] Append `.tmp` to complete filenames, declare guards before file handles, implement `ChunkerError::InvalidArgument`, run the quality gate and stop for review.

## Task 3: Manifest

Files: `core/src/manifest.rs`, `lib.rs`, `core/tests/manifest_test.rs`.

- [ ] Test supported version and algorithm, strict lowercase hashes, contiguous indexes and index-derived names, counts, exact sizes, checked overflow, valid positive sub-MiB chunk sizes and zero rejection.
- [ ] Deny unknown JSON fields on each structure; reject unsafe manifest names; require the empty BLAKE3 hash for zero-byte files. Load from a bundle or manifest path; guard and sync atomic manifest writes. Run gate and stop for review.

## Task 4a: Streaming and test helpers

Files: `core/src/stream.rs`, `core/src/lib.rs`, `core/tests/common/mod.rs`, `core/tests/stream_test.rs`.

- [ ] Test interrupted reads and propagation of other errors. Implement `#[doc(hidden)] pub fn read_retrying_unstable<R: std::io::Read + ?Sized>(reader: &mut R, buffer: &mut [u8]) -> std::io::Result<usize>` as a non-stable integration-test interface.
- [ ] Implement and test `is_cancelled` for absent, clear and set flags. Provide and test deterministic seeded data and a temp-directory helper using `tempfile`; retain guards. Run gate and stop for review.
- [ ] Put `#![allow(dead_code)]` in `core/tests/common/mod.rs`: this deliberately permits false-positive unused-helper warnings because each integration test compiles the shared module separately.

## Task 4b: Streaming split

Files: `core/src/split.rs`, `lib.rs`, `core/tests/split_basic_test.rs`.

- [ ] Test zero-byte input, exact and partial chunks, non-periodic input crossing 1 MiB buffer boundaries, public minimum rejection and both hashes.
- [ ] Expose `#[doc(hidden)] pub` non-stable `split_with_reader_unstable`, generic over `Read + Seek`, accepting the original filename, expected size, chunk size and buffer size plus output, cancellation and progress context. Validate the original filename using the plain-filename check, including a test rejecting an unsafe name. Integration tests use it with `common/`.
- [ ] Boundary sweep: buffer size 8, chunk sizes 7, 8, 9; source lengths 0, 1, chunk size minus 1, chunk size, chunk size plus 1, twice chunk size, and twice chunk size plus 1. Verify all bytes, sizes, chunk and master hashes, chunk count and final progress index. Use independent temporary directories.
- [ ] Use a deterministic growing reader to verify growth error, no final manifest and temporary cleanup. Cover early EOF and interrupted reads.
- [ ] Trigger cancellation by setting the token from a progress callback after at least one chunk is finalized; assert cancellation, no manifest, completed chunks retained and no `.tmp` files.
- [ ] Implement fixed-buffer splitting, retrying reads, cancellation, checked counts, `div_ceil`, `u64::min` before `usize` casts, atomic guarded writes and progress. Return `SplitSummary { bundle_path, reused_chunks: 0 }`; invalid public chunk size returns `InvalidArgument`. Run gate and stop for review.

## Task 5: Checkpointed resume

Files: `core/src/split.rs`, `core/tests/split_resume_test.rs`.

- [ ] Test same-size corrupted middle chunk, missing/truncated chunks, all chunks valid without a manifest, cancellation during resume, changed source and changed chunk size. Assert restored content and accurate `reused_chunks`; valid earlier chunks are reused and invalid later chunks rebuilt.
- [ ] Test re-splitting with a stale `manifest.json`: observe that it is removed during processing before a replacement can be published; cancel and assert the stale manifest remains absent.
- [ ] Re-read source from byte zero; checkpoint the master hasher and position before each candidate, restore on mismatch, clean orphan chunks from the first invalid index and check disk space after resume validation. Run gate and stop for review.

## Task 6: Verify

Files: `core/src/verify.rs`, `lib.rs`, `core/tests/verify_test.rs`.

- [ ] Test missing, truncated, same-size corrupt and reordered chunks, invalid master hash with valid chunk hashes, zero-byte files and cancellation.
- [ ] Validate manifest, check chunk sizes, stream and retry interrupted reads, verify every chunk and master hash. Use cancellation and correct progress; run gate and stop for review.

## Task 7: Merge

Files: `core/src/merge.rs`, `lib.rs`, `core/tests/merge_test.rs`.

- [ ] Test round trip, zero-byte input, existing output with and without force, corruption, cancellation, cleanup and preservation of an existing destination on failure.
- [ ] Test an existing output directory resolves to that directory plus original filename; verify returned path and bytes. Test an explicit output file resolves exactly to that path, verifies returned path and bytes, and enforces overwrite protection.
- [ ] Test an explicit `-o` file path whose parent directory does not exist; expect a clear error and assert no output, temporary file or newly created parent directory is left behind.
- [ ] Validate, check space, stream and hash into guarded `.tmp`, sync and close. Without force re-check existence immediately before `fs::rename`; with force use rename and propagate errors. Document the remaining race. Run gate and stop for review.

## Task 8: CLI

Files: `cli/src/args.rs`, `signals.rs`, `terminal_progress.rs`, `main.rs`, `cli/tests/cli_test.rs`.

- [ ] Test help, invalid/overflowing chunk size, missing input exit 1, split/verify/merge, overwrite protection and cancellation. Use `CARGO_BIN_EXE_chunker`.
- [ ] Implement checked MiB conversion, progress, dual Ctrl+C and unit-tested `fn exit_code(result: &Result<(), ChunkerError>) -> i32`: success 0, `InvalidArgument` 2, operational errors 1, cancellation 130. Clap errors exit 2. Never display progress beyond total or 1/0. Run gate and stop for review.

## Task 9: Documentation

- [ ] Write `README.md` covering usage, layout, validation, resume, overwrite, cancellation, exit codes, filename-length limits and rename race. Explain that Linux-valid filename characters can be rejected for Windows compatibility. Record performance claims only after measurement. Run gate and stop for review.

## Task 10: Efficiency and stress validation

- [ ] Add an ignored large-file round trip with streamed deterministic data and streamed comparison; specify disk requirements. Normal tests do not require huge files.
- [ ] Measure release throughput, elapsed time and peak memory across input sizes; record OS, Rust version, storage, input and chunk sizes. Distinguish fixed I/O buffers from growing manifest metadata. Resume reads source and existing chunks. Run gate and stop for review.

## Task 11: CI and final review

- [ ] CI uses the pinned 1.85.0 toolchain with rustfmt and clippy, and runs format, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` on Windows and Linux, with macOS where available. Keep stress tests ignored in normal CI.
- [ ] Review all required cases in `AGENTS.md`, inspect for whole-file production reads and incorrect temporary naming, and report limitations. Stop for review.
