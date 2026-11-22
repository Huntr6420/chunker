<!--
Read AGENTS.md and docs/superpowers/STATUS.md. Then run git log --oneline -5 and git status. Tell me the current task, what is verified, and the next three actions. Do not write anything until I confirm.
-->

# Chunker status

## Last updated

2026-11-22. Task 8 CLI is complete. Code commit `e3628b2` is on local `main`
after a fast-forward merge from `task-8`. The final audit confirmed Task 8's
approval.

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
- Task 7 (merge) is committed at `ef75cdb`. The hard-link data-loss finding is closed.
- Task 8 (CLI) is committed at `e3628b2` after the Windows review.
- At the last transfer check, GitHub and `D:\backup\chunker.bundle` both had
  `main` at `0120473` (Task 7 status). Task 8 is available in this local
  checkout. To transfer it via a folder copy, include the hidden `.git` folder.
  Cargo can rebuild the generated `target` folder.

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
- Merge and verify tests share `BoundedReader` in `core/tests/common/mod.rs`.
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
- Merge opens `<destination>.tmp` with `create_new(true)` and arms its cleanup
  guard only after the open succeeds. A refused open writes no bytes, deletes
  nothing, and leaves both the occupied temp entry and the destination untouched.
- For `AlreadyExists`, the full displayed message is
  `I/O error: Temporary file already exists: <tmp path>. If it is a leftover from an earlier run, remove it and retry.`
  Other open errors retain the OS error. A directory at the temp path returned
  `PermissionDenied` on the reviewed Windows checkout.

## Task 7 review results

- Codex independently ran the restored gate on Windows. Formatting and Clippy
  passed, and all 171 workspace tests passed, including 43 merge and 30 verify tests.
- Antigravity's saved mutation results record 37 single-test failures across
  34 mutations. Codex independently replayed eight single-test runs for rows
  21, 25, 32, 33, and 34, verifying the restored production hash after each run.
  Use the saved mutation diagnostics and the independent gate result. The pasted
  report misstated the row 20 failure (one progress event) and several split test names.
- Coverage gaps include any Linux run, a symlink at the merge temp path, the
  effect of removing `sync_all` or `guard.disarm()`, the checked-add overflow
  path, and bundle metadata errors other than `NotFound`.

## Task 8 CLI handoff

- Split defaults to the input's parent via `safe_parent` (`.` for a bare name).
  An explicit `--output` selects the parent of the bundle. Merge passes output
  and force through to core. Test omitted output, an existing directory and an
  exact file path, plus overwrite refusal and forced replacement.
- Verify accepts a bundle directory. Use metadata to distinguish an existing
  non-directory from a missing path. For an existing file, verify and merge
  both display `error: Invalid argument: Bundle path must be a directory: <path>`
  and exit 2. A missing input or other filesystem error exits 1.
- Parse arguments before installing the signal handler. Split defaults to
  500 MiB. Its clap parser accepts whole MiB and converts to `u64` bytes with
  checked multiplication. Zero gives `chunk size must be at least 1 MiB`;
  conversion overflow gives `chunk size is too large`. Test 1 MiB and the
  adjacent conversion boundaries 17592186044415 (fits) and 17592186044416
  (overflows). For clap errors assert exit 2, empty stdout and the exact
  application-authored fragment where applicable. Test a bad flag, nonnumeric
  size and successful help separately.
- Unit-test `exit_code(&Result<(), ChunkerError>) -> i32`: success 0,
  `InvalidArgument` 2, `Cancelled` 130, other errors 1. Preserve typed core
  errors and print their display once with the `error: ` prefix, except for
  cancellation. The signal handler owns both notices from spec §4.8. The first
  press sets the token; the second force-exits 130. Test both decisions without
  signals or timing. Use `anyhow` for setup. Handler installation failure exits
  1 before filesystem work and prints this line to stderr:
  `error: Could not set up Ctrl+C handling, so nothing was started. Reason: <cause>`.
- On success, stdout gets `Created bundle: <bundle path>`, `Merged file:
  <destination path>` or `Verified bundle: <bundle path>`, each on one line
  with a trailing newline. Use core's returned split/merge path and the supplied
  verify path, preserving their spelling. Split adds `Reused chunks: <n>` on
  a second line only if `SplitSummary.reused_chunks > 0`.
- Progress goes to terminal stderr. Piped success has empty stderr. Set the
  bar to cumulative `bytes_processed`, including verified reused chunks.
  Clamp display values only. Empty operations draw no bar; unit-test a
  synthetic zero-total event to guard against displaying `1/0`. Handle
  fallible progress setup without panicking. D8 option 1 was approved: use
  `writeln!` with stdout/stderr handles and handle write results. Keep the
  existing AGENTS.md scan for all in-scope Rust files.
- Use the five files listed in plan Task 8, private module unit tests and
  `cli/tests/cli_test.rs` for real-binary tests via `CARGO_BIN_EXE_chunker`.
  Use the existing dependencies. Put Context7 guidance in the builder prompt
  with versioned official docs as fallback. The spec and plan govern behavior.
- Resume fixture: give the binary an absolute input path to `sample.bin`
  containing 1 MiB of zeros followed by `0xA5`. Split at 1 MiB. Run 1 must
  print exactly `Created bundle: <bundle path>\n`. Delete only
  `part-0001.chk`, leaving the manifest in place for run 2. Run the same
  command again. Expect exactly `Created bundle: <bundle path>\nReused chunks: 1\n`.
  Derive the path from the fixture, require exit 0 and empty stderr both times,
  and use `File::open` plus `read_to_end` to check the restored chunk is exactly
  `[0xA5]`. A zero-byte split also prints only the first line.

## Task 8 review and next steps

- The follow-ups closed the review findings. The Ctrl+C test observes the
  cancellation token during the first notice write, pre-cancelled verify
  returns before path validation, and cancellation reporting writes zero
  diagnostic bytes. Real-binary tests check full application diagnostics.
  Progress initialization uses `get_or_insert_with`, and successful split,
  merge, and verify all use the private exit-code mapper through `report_result`.
- Codex independently ran the restored Windows gate in a scratch copy. All 197
  workspace tests passed (171 core, 17 CLI unit, 9 CLI integration). The
  forbidden-pattern scan was clean. Codex replayed row 1 with a fresh build
  directory. The real-binary zero-byte split test received exit 1 instead of 0
  when the mapper was mutated. The restored hash matched the reviewed source.
- After the merge, Codex reran all 26 CLI tests on Windows (17 unit, 9
  integration). The audit confirmed the exact §4.8 notices, per-test signal
  state, and production stderr terminal detection. The code commit contains
  exactly five CLI files. The Cargo manifests, lockfile, and `core/` match the
  Task 8 baseline.
- Two fixture setup splits, in the merge and verify integration tests, assert
  exit 0 only. Their stdout and stderr checks remain optional test cleanup.
  Live rendering in an interactive terminal remains unverified. Unit tests
  cover terminal gating, zero totals, and progress clamping.
- The Task 8 evidence record is archived at `D:\backup\task8-followup.log`
  and `D:\backup\task8-followup2.log`. Both copies match the SHA-256 hashes
  of the original captured logs in `%TEMP%`. The pasted report blocks contain
  incorrect test names.
- Carry these five verification gaps into Task 11 CI and final review work.
  Linux/macOS execution, real signal delivery, real handler-install failure,
  a real closed stdout, and bundle preservation after a stdout failure.
  The three core surplus-counter overflow sites also lack fixtures.

1. Review split and manifest temp-file hardening as a separate task.
2. Start the Task 9 README after that review and follow-up are complete.

## Known limits for the README (Task 9)

- Concurrent splits into the same bundle are unsupported (see plan Task 9).
- Some legal Linux filename characters are rejected for Windows compatibility.
  Filesystem filename length limits also apply (see plan Task 9).
- Merge without `--force` has a small race between the final existence check
  and rename (see plan Tasks 7 and 9). The Task 7 review note on rename says
  only that replacement behavior depends on the platform.
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

- Review temp-file creation in split and manifest save after Task 8 and before
  the Task 9 README, as a separate task. Both
  still call `File::create`, which truncates an existing file at the temp path.
- Remove the misplaced cancellation comment from the general diagnostic write
  in `cli/src/main.rs` (`report_result`) during a later cleanup. It can be
  bundled into the temp-file follow-up if that prompt explicitly includes it.
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

- Linux and macOS test results remain unverified.
- Decisions present only in chat cannot be verified from the repository.
