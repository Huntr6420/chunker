<!--
Read AGENTS.md and docs/superpowers/STATUS.md. Then run git log --oneline -5 and git status. Tell me the current task, what is verified, and the next three actions. Do not write anything until I confirm.
-->

# Chunker status

## Last updated

2026-11-22. Task 7 implementation baseline: `d5f90c00c3c994ef571fe56cac995b71dad60a18` on `task-7`.

## Current state

- Task 1 committed at `cc4c173` (workspace, toolchain, dependencies).
- Task 2 committed at `d2000f5`, with filename tests added in `68ed3ed`.
- Task 3 committed at `c0710eb` (manifest validation and atomic writes).
- Task 4a committed at `63045af` (streaming and test helpers).
- Task 4b committed at `aa49813` (streaming split). Plan commits `289d887`
  and `ac5a68d` record its decisions and follow-up requirements.
- Task 5 committed at `8f14246` (checkpointed split resume) and merged into `main`.
- Task 6 committed at `14ec3de` (verify) and is on `main`.
- Codex found the Task 6 review findings closed. Antigravity reported a passing
  Cargo gate with 30 verify tests. Codex independently checked formatting,
  whitespace, the forbidden-pattern scan, and all four final file hashes.
- The agreed test and mutation conventions were added to `AGENTS.md` in
  `8d7ab45`.
- Task 7 (merge) is implemented and under review on `task-7`, based on
  the docs-only handoff commit `d5f90c0`. The Rust changes are uncommitted and
  remain in the local working tree.
- **Hold the Task 7 code commit.** Codex reproduced destination data loss
  when a pre-existing `.tmp` hard link refers to the destination. The latest
  Antigravity report refreshed test evidence. The production fix is pending.

## Decisions to remember

- Session handoff ownership and update rules are recorded in `AGENTS.md`.
- Antigravity builds one task per chat on branch `task-N` and never commits. Codex plans, reviews, and edits only docs. The owner runs the gate and commits code and docs separately, then merges with `--ff-only`.
- Follow "Test and mutation conventions" in `AGENTS.md` for prompt drafting,
  deterministic tests, mutation restoration, and the six report items.
- Errors found before cancellation is observed take precedence. Once manifest save returns `Ok`, never return `Cancelled` (see plan Task 4b, inherited by Task 5).
- Use PowerShell. Running PowerShell syntax in `cmd` can create junk files in the repository root.

## Task 7 approved decisions

- With no explicit output, place the validated original filename beside the
  bundle. Merge accepts a bundle directory.
- Add a hidden reader/writer seam for merge streaming and hashing. The public
  wrapper owns the output file and its finalization.
- Move `BoundedReader` into `core/tests/common/mod.rs` for reuse. The owner
  approved edits to that file and `core/tests/verify_test.rs` for the move.
- After each expected payload, probe once for surplus. Keep the existing
  `SizeMismatch` message and report the observed count as a lower bound.
  If the probe finds surplus, the size error takes precedence over cancellation.
- Check cancellation at public and seam entry, at the top of payload read
  iterations, and immediately before rename, after sync, close, and the no-force
  destination recheck. Return success after a successful rename.
- Report a missing output parent as `Io(NotFound)` before checking space or
  opening a temporary file. An empty merge creates an empty output and emits
  no progress events.
- Check space for the full original size with zero reusable bytes and zero
  overhead. The spec and Task 7 plan record these decisions.

## Task 7 review handoff

- Local Rust changes are limited to `core/src/merge.rs`, `core/src/lib.rs`,
  `core/tests/merge_test.rs`, `core/tests/common/mod.rs`, and
  `core/tests/verify_test.rs`. The verify test edit moves `BoundedReader` to
  the shared test module. The status-only push does not include these Rust changes.
- Antigravity's latest evidence pass reported 31 exact-test mutation failures
  with restoration hashes, plus a passing gate with 39 merge tests and 30
  verify tests. Codex matched the current file hashes and checked formatting,
  whitespace, and the forbidden-pattern scan. Codex independently ran Clippy
  and all 167 workspace tests on the earlier test snapshot with the same
  production code. Codex has not independently replayed the 31 mutations.
- Reviewed `core/src/merge.rs` SHA-256:
  `9888DBB8BCFE4270C8776136150A6EFB64E36F0D55C81116E7ECDCF7CCA12550`.
  Latest `core/tests/merge_test.rs` SHA-256:
  `AA4F12E88D5F28E580B580067D394AF6566E09580737BAA40AC578746489C338`.
  `core/src/verify.rs` is unchanged. Compare its raw start and end hashes on
  the same checkout because Git line endings can change that hash.
- Reproduced on Windows with the actual merge function in a scratch directory.
  The existing output contained `previous contents`, and its `.tmp` path was a
  hard link to that same file. A forced merge returned `HashMismatch` for a
  corrupt chunk, but the old output contained `bad!` afterward. The temp open in
  `core/src/merge.rs` uses `create(true).truncate(true)`, which truncates the
  shared file before integrity checks. The guard then removes the pre-existing
  temp name. The accepted no-force rename race is a separate issue.

### Next three actions

1. Have Antigravity write a failing regression test for an occupied temp path,
   including the hard-link case. Use `OpenOptions::create_new(true)` to acquire
   a fresh temp file. Declare an unarmed guard before the file handle and arm it
   only after creation succeeds. Assert that a rejected merge leaves both the
   existing destination bytes and the occupied temp path intact.
2. Tighten the disk-space and rename-failure tests. Assert the full free-space
   message. Compare the rename error with one from an equivalent scratch rename,
   so an earlier I/O failure cannot pass the test. Keep the follow-up scoped to
   `core/src/merge.rs` and `core/tests/merge_test.rs`.
3. Rerun the affected mutations and the restored Cargo gate. Report the actual
   test failures, restore hashes, gate output, and final diff. Review again
   before the owner commits Task 7. Start Task 8 after that handoff.

## Known limits for the README (Task 9)

- Concurrent splits into the same bundle are unsupported (see plan Task 9).
- Some legal Linux filename characters are rejected for Windows compatibility.
  Filesystem filename length limits also apply (see plan Task 9).
- Merge without `--force` has a small race between the final existence check
  and rename (see plan Tasks 7 and 9).
- I/O buffers are fixed. Manifest metadata grows with the number of chunks,
  so there is no fixed ceiling on total memory (spec rev 2 §2).
- Non-UTF-8 paths are rejected.
- `load_manifest` treats any existing file path as a manifest. For nonempty
  bundles, pass the bundle directory to `verify_bundle`, which joins chunk
  names onto the original argument.
- If cancellation is observed after surplus bytes are seen during verification,
  `SizeMismatch` reports the byte count observed so far. That count is a lower
  bound on the stream length.
- The parent directory is not synced after rename.
- Watcher-thread cancellation tests depend on timing and must be rechecked on Linux CI.
- `source_n != chunk_n` in the verification loop is an equivalent mutant (slice comparison covers it).
- Zero-byte free-space check is untested.

## Open items

- `clean_orphan_chunks` accepts names like `part-1.chk`. Consider enforcing canonical names while preserving indexes above 9999.
- Consider using `rfind` in `extract_filename`.
- The chunk name format `part-{:04}.chk` appears in several places. Consider
  using a shared helper later.
- Verification has no Windows symlink test.
- Add Linux/macOS CI (see plan Task 11) and a `.gitattributes` with `* text=auto eol=lf`.

## Future (not in v1)

- After the chunker is built, add a storage layer across multiple Google Drive
  accounts. Split files, upload each chunk to an account with free space, and
  record where it lives so chunks can later be downloaded and rejoined.
  Defer design and implementation until after the chunker is built.

## Facts I could not verify

- Linux or macOS test results and live remote branch state were not checked.
- Decisions present only in chat cannot be verified from the repository.
