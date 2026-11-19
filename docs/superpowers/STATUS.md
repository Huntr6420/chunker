<!--
Read AGENTS.md and docs/superpowers/STATUS.md. Then run git log --oneline -5 and git status. Tell me the current task, what is verified, and the next three actions. Do not write anything until I confirm.
-->

# Chunker status

## Last updated

2026-11-18 at HEAD `ac5a68d6a16680a88cb167475d33a8347586f535`.

## Current state

- Task 1 committed at `cc4c173` (workspace, toolchain, dependencies).
- Task 2 committed at `d2000f5`, with filename tests added in `68ed3ed`.
- Task 3 committed at `c0710eb` (manifest validation and atomic writes).
- Task 4a committed at `63045af` (streaming and test helpers).
- Task 4b committed at `aa49813` (streaming split). Plan commits `289d887`
  and `ac5a68d` record its decisions and follow-up requirements.
- Current branch `task-5` starts at `ac5a68d`, also the local `main` tip.
  Checkpointed resume and cleanup changes are in the working tree. Modified
  files are `core/src/fs_utils.rs`, `core/src/split.rs`,
  `core/tests/common/mod.rs`, and `core/tests/split_basic_test.rs`.
  `core/tests/split_resume_test.rs` is untracked. The plan has an uncommitted
  Task 5 update. No Task 5 commit appears in the log.
- Pending for Task 5: review the diff, verify the full gate, and let the
  owner commit the approved changes.
- Next task after Task 5 review and commit: Task 6, verify.

## Decisions to remember

- Session handoff ownership and update rules are recorded in `AGENTS.md`.
- Antigravity builds one task per chat on branch `task-N` and never commits. Codex plans, reviews, and edits only docs. The owner runs the gate and commits code and docs separately, then merges with `--ff-only`.
- For every added guard or check, remove it temporarily and name the test that fails, then restore it. If no test fails, add one.
- Reviews require the initial failing output, a mutation table, full gate output, `git status --short`, and changed source regions. A summary line alone is insufficient.
- Errors found before cancellation is observed take precedence. Once manifest save returns `Ok`, never return `Cancelled` (see plan Task 4b, inherited by Task 5).
- Use PowerShell. Running PowerShell syntax in `cmd` can create junk files in the repository root.

## Known limits for the README (Task 9)

- Concurrent splits into the same bundle are unsupported (see plan Task 9).
- Some legal Linux filename characters are rejected for Windows compatibility.
  Filesystem filename length limits also apply (see plan Task 9).
- Merge without `--force` has a small race between the final existence check
  and rename (see plan Tasks 7 and 9).
- I/O buffers are fixed. Manifest metadata grows with the number of chunks,
  so there is no fixed ceiling on total memory (spec rev 2 §2).
- Non-UTF-8 paths are rejected.
- `load_manifest` treats any existing file path as a manifest.
- The parent directory is not synced after rename.
- Watcher-thread cancellation tests depend on timing and must be rechecked on Linux CI.

## Open items

- `source_n != chunk_n` in the verification loop is an equivalent mutant (slice comparison covers it); zero-byte free-space check is untested.
- Task 5 follow-up (requested by the reviewer; the owner must confirm whether it was sent to Antigravity; report pending):
  - Confirm the zero-byte path still has its own `check_free_space`.
  - Take the minimum in `u64` before casting to `usize` in the verification loop.
  - Add `read_full` and test that short reads from either the source or the chunk do not cause mismatches (see plan Task 5).
  - Remove the unused `_origin_pos`.
  - Run `test_cancellation_pre_save_check_after_orphan_cleanup` and the zero-byte pre-save test 50 times each.
- `clean_orphan_chunks` accepts names like `part-1.chk`. Consider enforcing canonical names while preserving indexes above 9999.
- Consider using `rfind` in `extract_filename`.
- Add Linux/macOS CI (see plan Task 11) and a `.gitattributes` with `* text=auto eol=lf`.
- Review Task 5 rollback, reuse counts, progress, cancellation, and both orphan
  cleanup stages against the current plan. Verify cleanup tolerates `NotFound`
  and propagates other removal errors.
- Check that Task 5 changes existing Task 4b tests only in how orphan fixtures
  are created, as specified in the plan.
- Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` before asking the owner to commit Task 5.

## Facts I could not verify

- Task 5 gate results and review approval. This handoff records files and Git
  history. It does not attest that the in-progress implementation passes.
- Linux or macOS test results and live remote branch state were not checked.
- Decisions present only in chat cannot be verified from the repository.
