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
2. Test first when implementing or fixing behavior: write a failing test, see
   it fail, then write the code. For tests of existing behavior, see Mutation
   evidence below.
3. Before calling a task done, run all of:
   - `cargo fmt`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
4. Keep diffs small. Do not refactor unrelated code.
5. Add a dependency only if both the approved spec and the task prompt
   authorize it.
6. The owner handles merges and pushes. Do not run destructive commands
   (deleting folders, `git reset --hard`, force pushes) without asking.
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

## Test and mutation conventions

### Drafting prompts
- State the operation order wherever it affects results: entry check, open,
  metadata check, read, integrity checks, final check. Why: order decides which
  error or cancellation wins.
- Before a prompt goes to the builder, check every "must": can a test reach it,
  what does it change that a test can see, can it hang, does it conflict with
  another rule. Why: an unreachable rule produces a test that proves nothing.

### Writing tests
- Give each guard a test that reaches it. If another check can return the same
  error, also assert exact read, progress or opener counts, or a distinguishing
  field such as the file name. Why: otherwise a later check hides a deleted one.
- Assert the exact error variant and the full displayed message, built
  independently of the returned error. Why: comparing a message to itself cannot
  fail.
- Set cancellation tokens before the call, or from inside the injected reader or
  callback. New tests use no threads, sleeps or timing. Older tests are not
  rewritten unless a task says so. Why: the trigger must repeat every run.
- Give every injected reader a hard call limit that panics with a clear message.
  Expose the call count to the test when an exact count tells two paths apart
  (for example, proving cancellation prevented a second read). Why: a broken
  loop can spin forever, and an `io::Error` could be swallowed by the code under
  test.
- Fixtures must be able to show the property. A buffer test uses a chunk larger
  than `BUFFER_SIZE` (for example 2 MiB) and asserts on the recorded read-slice
  lengths, because the verifier caps each request at the bytes remaining. A
  read-only test proves the fixture denies a read+write open, then restores
  permissions. Surplus fixtures deliver one byte per call when counts matter.

### Mutation evidence
- A new test for behavior that is already correct may pass before the fix. Its
  mutation shows what it catches. Record the actual pre-fix outcome either way.
- Mutate one check at a time. Temporary edits to the production file are allowed
  for this. Back it up to `$env:TEMP`, restore it after each run, and compare
  `Get-FileHash` SHA-256 to the baseline before the next mutation and at the end.
- Run `cargo test -p <crate> --test <file> -- <name> --exact` and show that exactly
  one test ran. Do not run Clippy during mutations; run it after everything is
  restored. Compile failures, hangs and timeouts do not count as caught. Explain
  any genuine equivalent mutant.

### CLI output and tests

- See [Task 8 CLI handoff](docs/superpowers/STATUS.md#task-8-cli-handoff) for the
  approved messages, exit codes and resume fixture.
- Write CLI result lines to stdout and errors to stderr using `writeln!` on
  the appropriate handle. Handle the write result. Show progress only when
  stderr is a terminal. The forbidden-pattern scan also applies to CLI files.
- In `cli/tests/cli_test.rs`, run the binary through `CARGO_BIN_EXE_chunker`.
  Check the exit code and both streams. On piped success, compare all stdout,
  including the final newline, against expectations built from the fixture,
  and assert stderr is empty. For clap-wrapped validation errors, check the
  exact Chunker-authored message inside clap's diagnostic.
- Unit-test private exit-code, signal and progress helpers in their own modules.
  For a binary unit-test mutation, use
  `cargo test -p cli --bin chunker -- <fully-qualified-test-name> --exact`
  and confirm exactly one test ran.

### Reporting
- Scan the in-scope Rust files with `rg -n -- 'MUTATION|with_extension|println!|fs::read\('`.
  Exit 1 means clean. Keep these strings out of panic messages, test names and
  comments.
- Report: (1) pre-fix commands and results, (2) mutation table with named tests,
  failures and restore hashes, (3) full Cargo gate output and exit codes,
  (4) full `git status --short`, (5) scan command, output and exit code,
  (6) changed regions with paths.

### Builder limits
- The builder does not commit, merge, push, or edit `docs/`, `AGENTS.md` or
  `STATUS.md`. It adds a dependency only if both the approved spec and the task
  prompt authorize it. Out-of-scope needs are reported as blockers.

## Review mode
When asked to review (not build), do not edit files. List bugs, deviations from
the spec, and missing tests, ordered by severity, with file and line references.

## Session start

- Every agent reads `AGENTS.md`, then `docs/superpowers/STATUS.md`, then only
  the spec sections and plan task relevant to the work.
- Git history and the files on disk are the source of truth. If `STATUS.md`
  disagrees with them, report the discrepancy and ask before acting.
- Only the planner/reviewer (Codex) edits `STATUS.md`. Builders (Antigravity)
  never edit `docs/`, `AGENTS.md`, or `STATUS.md`.
- Codex updates `STATUS.md` after every task commit and whenever a decision is
  made that is absent from the spec and plan.
