# AGENTS.md: chunker project context

## What this project is
A Rust workspace that splits very large files (10 GB to 100+ GB) into fixed-size
chunks (default 500 MiB) and merges them back with cryptographic verification.
It is built as:

- `core/`: headless library. No terminal output, no UI crates. Synchronous
  streaming I/O, progress via an `FnMut(ProgressEvent)` callback, cancellation
  via `Arc<AtomicBool>`, RAII cleanup of temp files. A Tauri GUI will wrap this later.
- `cli/`: binary named `chunker` with commands `split`, `merge`, `verify`.
  Uses `clap`, `indicatif`, `anyhow`, `ctrlc`.

The owner is an MCA student, new to Rust, building this as a portfolio project.
After each task, explain briefly what you did and why, in plain language.

## Source of truth
- Approved spec: `docs/superpowers/specs/2026-11-16-chunker-design.md`
- Implementation plan: `docs/superpowers/plans/` (if present)

Follow the spec exactly. If you think something in it is wrong or unclear, stop
and say so. Do not silently change the spec or deviate from it.

## Working rules
1. Work on ONE plan task at a time, then stop and wait for review.
2. Test first: write a failing test, see it fail, then write the code.
3. Before calling a task done, run all of:
   - `cargo fmt`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
4. Keep diffs small. Do not refactor unrelated code.
5. Do not add dependencies beyond those listed in the spec without asking.
6. Do not run destructive commands (deleting folders, `git reset --hard`,
   force pushes) without asking.
7. Do not commit. The owner commits after reviewing each task.

## Hard design constraints (summary; the spec has full detail)
- Fixed 1 MiB I/O buffers; manifest metadata grows with chunk count. Never
  `fs::read` a whole file or chunk. Public split and CLI require chunks >= 1 MiB;
  manifest validation accepts any positive chunk size.
- Hashing: BLAKE3, one pass feeding both a per-chunk hasher and a master hasher.
- Chunk files are `part-0000.chk`, `part-0001.chk`, ... inside `<name>.chunks/`
  next to `manifest.json`. Chunk size units in the CLI are MiB.
- Temp files: always append `.tmp` (never `with_extension`). Write to `.tmp`,
  `sync_all()`, then atomically rename. Applies to chunks, manifest, merged output.
- `TmpFileGuard` deletes active `.tmp` files on error, cancel, or panic.
  Declare the guard BEFORE the file handle so the handle closes first (Windows).
- All read loops: propagate errors with `?`, retry on `ErrorKind::Interrupted`,
  and verify total bytes read equals the expected size.
- Split resume: re-read and hash the source from byte 0 (no seeking forward
  without hashing). Clone the master hasher before each candidate chunk; on a
  mismatch, restore the clone and seek the source back. Resume from the first
  missing or mismatched chunk.
- Merge/verify: check chunk file size before hashing; verify each chunk hash and
  the master hash; merge checks for an existing destination without `--force`
  before writing and again before rename, with a small remaining race.
- Security: reject manifest names that are not already plain safe filenames;
  split extracts the final component of an input path. Reject
  Windows reserved names like CON/NUL/COM1 using the part before the first dot,
  reject invalid characters and trailing dots/spaces. Chunk names come from the
  index, never trusted from the manifest. Manifest hashes must be 64-char
  lowercase hex. Use checked arithmetic on all manifest sizes.
- Exit codes: 0 success, 1 operational error (including missing input), 2 usage
  error, 130 cancelled.
  First Ctrl+C cancels gracefully, second force-exits with 130.
- 0-byte files are supported (0 chunks; master hash is the BLAKE3 of empty input).

## Environment
- Primary OS: Windows (PowerShell). Must also work on Linux and macOS.
- Rust edition 2024 (needs Rust 1.85 or newer). Check with `rustc --version`.
- Workspace root `Cargo.toml` has `members = ["core", "cli"]`.

## Out of scope for v1 (do not build)
Compression (zstd), content-defined chunking (FastCDC), parallel chunk I/O,
the Tauri GUI, encryption.

## Required tests
The implementation plan assigns required tests to tasks and to `core/tests/`
and `cli/tests/`; tests reach the hidden seams from `core/tests/`. The shared
`core/tests/common/mod.rs` uses `#![allow(dead_code)]` deliberately because each
integration test compiles the shared helpers separately. Cover round trip, 0-byte file, buffer-straddling
boundary, resume with rollback, manifest validation, path sanitization,
overwrite protection, corruption detection, verify master hash, cancel cleanup,
stale/orphan cleanup, file growth error, safe parent path. Also cover:
all chunks present but manifest missing, source size an exact multiple of the
chunk size, changed source or chunk size on resume, and cancellation during the resume pass. Tests use temp directories
and small chunk sizes; never require huge files.

## Review mode
When asked to review (not build), do not edit files. List bugs, deviations from
the spec, and missing tests, ordered by severity, with file and line references.
