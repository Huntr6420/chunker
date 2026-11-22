# Chunker

Chunker splits a file into smaller pieces and puts them in a bundle folder. You can verify the bundle on its own or merge it back into a file. Both operations check every chunk and the whole file against the BLAKE3 hashes recorded in the manifest.

For example, with `chunker` installed and `archive.iso` in the current folder, these commands create a bundle beside the original and restore it under a second name:

```powershell
chunker split archive.iso
chunker verify archive.iso.chunks
chunker merge archive.iso.chunks --output restored.iso
```

## Build and install

Run `cargo build --release` from the repository root. Both crates require Rust 1.85 or newer and use edition 2024. The repository selects Rust 1.85.0 in `rust-toolchain.toml`. With Cargo’s default target directory, the executable is `target\release\chunker.exe` on Windows and `target/release/chunker` on Linux and macOS.

To run `chunker` by name from other folders, use `cargo install --path cli --locked` from the repository root and ensure Cargo’s binary directory is on your `PATH`.

## Try a round trip

This PowerShell example uses the Windows release build. Start **in the repository root** after building. Save the executable’s path before switching to a new scratch folder:

```powershell
$chunker = (Resolve-Path .\target\release\chunker.exe).Path
$demo = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ([guid]::NewGuid()))
Push-Location $demo.FullName
```

Create a 3 MiB sample with a fixed seed, split it into three chunks, and verify and restore it:

```powershell
$bytes = New-Object byte[] (3 * 1024 * 1024)
[System.Random]::new(42).NextBytes($bytes)
[System.IO.File]::WriteAllBytes((Join-Path $PWD 'sample.bin'), $bytes)

& $chunker split sample.bin --chunk-size 1
& $chunker verify .\sample.bin.chunks
& $chunker merge .\sample.bin.chunks --output restored.bin
(Get-FileHash -Path sample.bin, restored.bin -Algorithm SHA256 -ErrorAction Stop).Hash
Pop-Location
```

Captured output on Windows, in command order:

```text
Created bundle: .\sample.bin.chunks
Verified bundle: .\sample.bin.chunks
Merged file: restored.bin
069750240DA6465B4F23FAF9635C973F6CC7D85718AB784FD0A1172D49E4080B
069750240DA6465B4F23FAF9635C973F6CC7D85718AB784FD0A1172D49E4080B
```

All three Chunker commands exited with code 0. The two SHA-256 lines come from PowerShell. In your own run, compare the hash of the original with the hash of the restored file. They should match each other. Chunker writes success lines to stdout and shows progress only when stderr is a terminal.

## Bundle layout

```text
sample.bin.chunks/
├── manifest.json
├── part-0000.chk
├── part-0001.chk
└── part-0002.chk
```

Keep these files together. The manifest records the original filename, file size, whole-file hash, chunk size, and the name, size, index, and hash of each chunk. The last chunk can be shorter than the rest. Pass the **bundle folder** to both `verify` and `merge`.

## Command reference

| Command | Arguments and defaults | Success output |
| --- | --- | --- |
| `chunker split <INPUT> [--output <DIR>] [--chunk-size <MiB>]` | Creates `<filename>.chunks` inside the output directory. The default is the input file’s parent, or the current folder for a bare filename. Chunk size is a whole number of MiB (1,048,576 bytes each), minimum 1 and default 500. | `Created bundle: <bundle path>`. When split reuses one or more chunks, it adds a second line, `Reused chunks: <n>`. |
| `chunker verify <BUNDLE>` | Reads the bundle and checks its integrity without creating a merged file. | `Verified bundle: <bundle path>` |
| `chunker merge <BUNDLE> [--output <PATH>] [--force]` | By default, writes the original filename beside the bundle. An existing output directory receives that filename. A file path selects the exact destination. Use `--force` to allow replacement of an existing file. | `Merged file: <destination path>` |

### What verification checks

The Rust library in `core/` does the file work for the CLI in `cli/`. Both verify and merge validate the manifest, including its version, filenames, chunk sequence, sizes, and hash format. Chunk filenames are derived from their indexes. Before reading each chunk, Chunker checks its file size. As it reads, it checks for short or surplus data and computes both BLAKE3 hashes in one pass over the chunk data. Each chunk hash and the final whole-file hash must match the manifest. Merge writes to a temporary file and only renames it to the destination after these checks pass.

### Resuming a split

Run the same split command again after an interruption. Split rereads the source from byte zero. For each consecutive chunk already on disk, it checks the expected size and compares the bytes with the source while hashing. It keeps the matching prefix and writes from the first missing or different chunk onward. If a comparison fails after reading has started, it restores the saved whole-file hash state and rereads from that chunk’s start.

For example, if the first two chunks match and the third is missing, split keeps the first two and prints `Reused chunks: 2` after the bundle path. If the source or chunk size changes, reuse follows the same size and byte checks using the new input and chunk boundaries. Split removes surplus chunk files beyond the new count.

### Cancellation and exit codes

The first Ctrl+C requests cancellation. When the operation observes it, cleanup attempts to remove its active temporary file and the command exits with code 130. A second Ctrl+C forces an immediate exit and can leave a temporary file behind. The respective notices on stderr are:

```text
[!] Cancellation requested. Cleaning up active temporary files... (Press Ctrl+C again to force exit)
[!] Force exiting immediately.
```

| Exit code | Meaning | Examples |
| --- | --- | --- |
| 0 | Success | The command completed or displayed help. |
| 1 | Operational error | Missing input, corrupt bundle, insufficient disk space, or an existing merge destination without `--force`. |
| 2 | Usage error | Unknown flag, invalid chunk size, or a regular file passed as the bundle to merge or verify. |
| 130 | Cancelled | The operation observed cancellation or a second Ctrl+C forced an exit. |

## File handling and limits

- **Temporary files.** Chunker writes chunks, manifests, and merged output to a sibling path ending in `.tmp`, syncs the file with `sync_all()`, closes it, then atomically renames it. On error, cancellation, or panic unwinding, the cleanup guard attempts to delete the temporary file it owns. The parent directory is not synced after rename.
- **Occupied temp names.** At startup, split removes recognized stale files in the bundle matching `part-*.chk.tmp` or named `manifest.json.tmp`. When a writer creates a temporary file, it requires an unused name. If the OS reports `AlreadyExists`, Chunker says to remove the entry only if it is a leftover from an earlier run. Other failures use the OS diagnostic. A directory at the temp path can produce “Access denied” on Windows.
- **Concurrent writes.** Concurrent splits into the same bundle are unsupported. Without `--force`, merge checks the destination before writing and again before renaming. If another process creates the destination after the final check, whether rename replaces it depends on the platform.
- **Filenames.** Split requires a UTF-8 input path. Chunker rejects some names that Linux allows so bundles can be used on Windows. Examples include `notes?.txt`, reserved device names such as `CON.txt`, and names ending in a space or dot. Filesystem filename and path length limits still apply, including to the added `.chunks` and `.tmp` suffixes.
- **Memory and empty files.** Chunk payloads are streamed using fixed 1 MiB I/O buffers. Manifest memory grows with the number of chunks. Splitting a zero-byte file creates a manifest with zero chunks and the BLAKE3 hash of empty input. Merging that bundle restores a zero-byte file.

## Verification status

The reviewed Windows run passed 199 workspace tests. Linux and macOS runs, real Ctrl+C delivery, and live progress rendering in an interactive terminal remain unverified.

Compression, content-defined chunking, parallel chunk I/O, a GUI, and encryption are outside the scope of v1.
