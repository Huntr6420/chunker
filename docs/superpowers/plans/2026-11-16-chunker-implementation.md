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
- [ ] Put `#![allow(dead_code)]` in `core/tests/common/mod.rs`: this deliberately permits false-positive unused-helper warnings because each integration test compiles the shared module separately.
- [ ] Implement and test `is_cancelled` for absent, clear and set flags. Provide and test deterministic seeded data and a temp-directory helper using `tempfile`; retain guards. Run gate and stop for review.

## Task 4b: Streaming split

Files: `core/src/split.rs`, `core/src/lib.rs`, `core/tests/split_basic_test.rs`, `core/tests/common/mod.rs`.

### Decisions and interfaces

Task 4b performs a fresh split. `reused_chunks` is always 0; no checkpointing or reuse belongs here. Split creates `output_dir` and `<output_dir>/<name>.chunks` using `create_dir_all`; merge still never creates parents.

The exact non-stable seam signature is in the spec's Public API: `reader: &mut R` (`Read + Seek`), `original_filename: &str`, `expected_size: u64`, `chunk_size: u64`, `buffer_size: usize`, `output_dir: &Path`, cancellation token, then progress callback. Keep `Seek` even though Task 4b does not seek. Validate the filename with `validate_plain_filename`. Reject zero chunk or buffer sizes with `InvalidArgument`. Only the public wrapper enforces `MIN_CHUNK_SIZE`.

`split_file` validates public chunk size, extracts the input filename, opens the source, gets its size and delegates with `BUFFER_SIZE`. Missing input returns `Io`. Both paths return `SplitSummary`.

The Public API and this plan take precedence over spec 4.6's illustrative samples: return `SplitSummary`, use `read_retrying_unstable`, and check cancellation before reads rather than copying the sample's read-first ordering.

### Startup, streaming and finalization

- Validate arguments and check cancellation at entry, including the zero-byte path. Create output and bundle directories. Remove old `manifest.json`, propagating removal errors except `NotFound`, then call `clean_stale_tmp_files`.
- Before writing any chunk, call `check_free_space(&bundle_dir, expected_size, 0, 1024 * 1024)`; overhead is 1 MiB. Task 5 later moves this check after resume validation.
- Zero-byte input takes an explicit early path: no chunk files, no progress events, total_chunks 0 and the empty BLAKE3 hash. It still uses the cancellation and finalization checks below; do not evaluate `total_chunks - 1`.
- Calculate chunk count with `expected_size.div_ceil(chunk_size)` and checked `usize::try_from`. Create each chunk temporary file lazily only when bytes remain; an exact multiple never creates an empty extra chunk. Apply `u64::min` before casting a slice length to `usize`.
- Reset the chunk hasher only when opening a new temporary chunk. Feed both hashers exactly the slice written; every byte enters the master hasher once. Declare the guard before the writer, sync and close before rename, and disarm only after rename succeeds.
- Detect growth with checked byte arithmetic before hashing or writing the crossing read; return `Io(InvalidData)`. A short source returns `Io(UnexpectedEof)`. Both errors leave no manifest or active `.tmp`; completed chunks remain.
- After consuming the expected bytes and checking for early EOF, probe once with `read_retrying_unstable` into a one-byte buffer. The zero-byte path makes the same probe. A nonzero result returns the same `Io(InvalidData)` error with `Input file grew during split`. Propagate other read errors. This catches extra data when the read loop reaches `expected_size` exactly and stops without seeing the extra bytes.
- Check `is_cancelled` at entry, at the top of every read-loop iteration, immediately before `clean_orphan_chunks`, and immediately before `save_manifest_atomic`. The zero-byte path has three checkpoints: entry, before cleanup, and before save. A growth or I/O error found before cancellation is observed takes precedence over `Cancelled`. Observed cancellation keeps completed chunks, publishes no new manifest and leaves no active `.tmp`. This includes cancellation from the final progress callback when the probe succeeds.
- Once the byte length and chunk count are verified, call `clean_orphan_chunks(&bundle_dir, total_chunks)` before saving the manifest, so smaller re-splits remove surplus old chunks. Task 5 instead cleans from the first invalid index before writing.
- Build the manifest value before the final cancellation check, then call `save_manifest_atomic` immediately after that check. That save has no cancellation parameter: a flag arriving during the save is not guaranteed to stop publication. Once save returns `Ok`, return success without another cancellation check. Flags set after return cannot undo completion.

### Tests and implementation passes

Build in two passes; tests precede implementation in each pass. First implement happy paths and the boundary sweep; then add growth, early EOF, interrupted reads and cancellation cases. Do not invent failures for already-working behavior; report actual expected failures accurately.

- [ ] Test public sub-minimum chunk size -> `InvalidArgument`, missing input -> `Io`, seam zero buffer/chunk sizes -> `InvalidArgument`, and unsafe seam filename -> `InvalidFilename`.
- [ ] Test split creates a missing nested output directory and the bundle. Test startup removes stale manifest and recognized temporary files while preserving unrelated files; removal errors other than `NotFound` must propagate.
- [ ] Test zero-byte success: zero chunks, empty hash, no events, valid manifest, no temporary files. Test an already-set token on zero-byte input: `Cancelled` and no manifest written.
- [ ] Boundary sweep: buffer 8; chunks 7, 8, 9; lengths 0, 1, chunk-1, chunk, chunk+1, 2*chunk, 2*chunk+1. Use a separate tempdir per case. Independently derive expectations from source bytes, without implementation helpers; verify concatenated bytes, each size/hash, master hash, count, `validate_manifest`, no `.tmp`, and final progress (or no events for empty input).
- [ ] Test a read spanning several chunks with a buffer larger than multiple chunk sizes. Assert that the progress event reports the chunk containing the last byte of the read and the correct cumulative byte count. Test exact multiples produce a full final chunk and no empty extra chunk.
- [ ] Test progress at exact boundaries reports the chunk just finished, never index `total_chunks`; byte counters never decrease and finish at total bytes with index `total_chunks - 1`.
- [ ] Real-buffer case: chunk size 1,048,577, 1 MiB buffer, seeded non-periodic source longer than two chunks. Verify full output and both hash levels.
- [ ] Re-split a larger then smaller source under the same name/output: second content and manifest match the new source, no orphan chunks remain, and `reused_chunks == 0`.
- [ ] Add interrupting/growing/shrinking reader wrappers implementing `Read + Seek` in `common/mod.rs`. Simulate growth with data exceeding expected size; arrange completed chunks before the crossing read and prove that read was not written. Test early EOF, interrupted-read retries, no manifest, no `.tmp`, completed chunks retained.
- [ ] Test buffer-aligned growth (expected 16 bytes, buffer 8 bytes, source 24 bytes) and an expected size of zero with a non-empty source. Both must report `Io(InvalidData)` with the growth message and publish no manifest. A source whose length exactly matches `expected_size` must succeed.
- [ ] Cancellation A: callback sets token after a chunk finishes with more data to read; expect `Cancelled`, completed chunks retained, no manifest or `.tmp`.
- [ ] Cancellation B: callback sets token on the final progress event; expect `Cancelled`, all completed chunks retained, no manifest or `.tmp`.
- [ ] Pre-create a surplus chunk and cancel from the final progress callback. Assert `Cancelled`, the surplus chunk is retained, and no manifest or `.tmp` remains.
- [ ] Cancellation C: set token only after `split_file` returns `Ok`; result and saved bundle remain successful and unchanged.
- [ ] Cover all four cancellation checkpoints in the normal path and all three in the zero-byte path. Each checkpoint needs at least one test that fails if only that check is removed. To isolate the zero-byte pre-cleanup check, pre-create a surplus chunk and use a reader whose `read()` sets the token during the probe and returns `Ok(0)`. Assert `Cancelled`, the surplus chunk is retained, and no manifest or `.tmp` remains.
- [ ] Run targeted tests and the full quality gate; stop for review without committing.

## Task 5: Checkpointed resume

Files: `core/src/split.rs`, `core/tests/split_resume_test.rs`.

- [ ] Test same-size corrupted middle chunk, missing/truncated chunks, all chunks valid without a manifest, cancellation during resume, changed source and changed chunk size. Assert restored content and accurate `reused_chunks`; valid earlier chunks are reused and invalid later chunks rebuilt.
- [ ] Place a stale `manifest.json`, start a re-split, cancel partway, and assert `manifest.json` does not exist.
- [ ] When reworking cleanup, make `clean_orphan_chunks` and `clean_stale_tmp_files` propagate `remove_file` errors other than `NotFound`. Test both helpers for propagation of other removal errors and tolerance of `NotFound`.
- [ ] Re-read the source from byte zero. Checkpoint the master hasher and reader position before each candidate and restore both on mismatch. For each comparison, fill both buffer slices to the requested length, bounded by the bytes remaining in the chunk. Accumulate short reads and retry `Interrupted` so short reads alone do not cause a mismatch. Handle actual EOF and other I/O errors separately.
- [ ] During resume, emit one progress event per verified chunk and none for rejected candidates. `bytes_processed` must never decrease, including across the transition to writing.
- [ ] After resume verification, clean from the first invalid index before writing and check disk space using the verified prefix. A re-split with a different source overwrites existing chunks from the first mismatch onward. Keep the end-of-split orphan cleanup at `total_chunks` after the write phase and before saving the manifest.
- [ ] Existing Task 4b tests may change only in how they create orphan fixtures. Create those orphans inside the final progress callback so they exercise the end-of-split cleanup. Keep the existing assertions.
- [ ] Run the gate and stop for review.

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

- [ ] Write `README.md` covering usage, layout, validation, resume, overwrite, cancellation, exit codes, filename-length limits and rename race. List concurrent splits into the same bundle as unsupported. Explain that Linux-valid filename characters can be rejected for Windows compatibility. Record performance claims only after measurement. Run gate and stop for review.

## Task 10: Efficiency and stress validation

- [ ] Add an ignored large-file round trip with streamed deterministic data and streamed comparison; specify disk requirements. Normal tests do not require huge files.
- [ ] Measure release throughput, elapsed time and peak memory across input sizes; record OS, Rust version, storage, input and chunk sizes. Distinguish fixed I/O buffers from growing manifest metadata. Resume reads source and existing chunks. Run gate and stop for review.

## Task 11: CI and final review

- [ ] CI uses the pinned 1.85.0 toolchain with rustfmt and clippy, and runs format, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` on Windows and Linux, with macOS where available. Keep stress tests ignored in normal CI.
- [ ] Review all required cases in `AGENTS.md`, inspect for whole-file production reads and incorrect temporary naming, and report limitations. Stop for review.
