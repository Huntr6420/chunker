<!--
Read AGENTS.md and docs/superpowers/STATUS.md. Then run git log --oneline -5 and git status. Tell me the current task, what is verified, and the next three actions. Do not write anything until I confirm.
-->

# Chunker status

## Last updated

2026-11-20 at HEAD `14ec3def815261366ed43e931b1de81e92859483`.

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
- Next task is Task 7 (merge), after the owner approves the handoff.

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
