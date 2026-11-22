use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

fn chunker_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_chunker"))
}

fn read_file_bytes(path: &Path) -> Vec<u8> {
    let mut file = File::open(path).expect("failed to open file");
    let mut data = Vec::new();
    file.read_to_end(&mut data).expect("failed to read file");
    data
}

#[test]
fn test_cli_split_omitted_and_explicit_output() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("input.bin");
    std::fs::write(&input_path, b"test split data for omitted output").unwrap();

    // Case 1: omitted output (bundle beside input)
    let output1 = chunker_bin()
        .args(["split", input_path.to_str().unwrap(), "--chunk-size", "1"])
        .output()
        .unwrap();

    let bundle1 = temp.path().join("input.bin.chunks");
    assert_eq!(output1.status.code(), Some(0));
    assert!(output1.stderr.is_empty());
    let expected_stdout1 = format!("Created bundle: {}\n", bundle1.display());
    assert_eq!(String::from_utf8_lossy(&output1.stdout), expected_stdout1);
    assert!(bundle1.is_dir());

    // Case 2: explicit output directory
    let out_dir = temp.path().join("explicit_out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let output2 = chunker_bin()
        .args([
            "split",
            input_path.to_str().unwrap(),
            "--output",
            out_dir.to_str().unwrap(),
            "--chunk-size",
            "1",
        ])
        .output()
        .unwrap();

    let bundle2 = out_dir.join("input.bin.chunks");
    assert_eq!(output2.status.code(), Some(0));
    assert!(output2.stderr.is_empty());
    let expected_stdout2 = format!("Created bundle: {}\n", bundle2.display());
    assert_eq!(String::from_utf8_lossy(&output2.stdout), expected_stdout2);
    assert!(bundle2.is_dir());
}

#[test]
fn test_cli_split_default_folder_and_size() {
    let temp = tempfile::tempdir().unwrap();
    let input_dir = temp.path().join("input_dir");
    let work_dir = temp.path().join("work_dir");
    std::fs::create_dir_all(&input_dir).unwrap();
    std::fs::create_dir_all(&work_dir).unwrap();

    let input_path = input_dir.join("sample.bin");
    std::fs::write(&input_path, b"sample content").unwrap();

    // Run chunker from a different directory than the input
    let output = chunker_bin()
        .current_dir(&work_dir)
        .args(["split", input_path.to_str().unwrap()])
        .output()
        .unwrap();

    let bundle_path = input_dir.join("sample.bin.chunks");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let expected_stdout = format!("Created bundle: {}\n", bundle_path.display());
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected_stdout);
    assert!(bundle_path.is_dir());

    // Check that omitting --chunk-size produces exactly 500 * 1_048_576 bytes
    let manifest = chunker_core::load_manifest(&bundle_path).unwrap();
    assert_eq!(manifest.chunk_size_bytes, 500 * 1024 * 1024);
}

#[test]
fn test_cli_missing_input_split() {
    let temp = tempfile::tempdir().unwrap();
    let missing_path = temp.path().join("non_existent_file.bin");

    let output = chunker_bin()
        .args(["split", missing_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let os_error = File::open(&missing_path).unwrap_err();
    let expected_err = format!("error: I/O error: {os_error}\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), expected_err);
}

#[test]
fn test_cli_merge_round_trip_and_modes() {
    let temp = tempfile::tempdir().unwrap();
    let orig_data = b"hello world merge test content bytes";
    let input_path = temp.path().join("test_input.bin");
    std::fs::write(&input_path, orig_data).unwrap();

    let split_out_dir = temp.path().join("split_dir");
    std::fs::create_dir_all(&split_out_dir).unwrap();

    let split_res = chunker_bin()
        .args([
            "split",
            input_path.to_str().unwrap(),
            "--output",
            split_out_dir.to_str().unwrap(),
            "--chunk-size",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(split_res.status.code(), Some(0));
    let bundle_path = split_out_dir.join("test_input.bin.chunks");
    assert_eq!(
        String::from_utf8_lossy(&split_res.stdout),
        format!("Created bundle: {}\n", bundle_path.display())
    );
    assert!(split_res.stderr.is_empty());

    // Case 1: Default merge (no flags), destination is absent before merge
    let default_dest = split_out_dir.join("test_input.bin");
    assert!(!default_dest.exists());

    let merge1 = chunker_bin()
        .args(["merge", bundle_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(merge1.status.code(), Some(0));
    assert!(merge1.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&merge1.stdout),
        format!("Merged file: {}\n", default_dest.display())
    );
    assert_eq!(read_file_bytes(&default_dest), orig_data);

    // Case 2: Overwrite refusal without --force
    let merge2 = chunker_bin()
        .args(["merge", bundle_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(merge2.status.code(), Some(1));
    assert!(merge2.stdout.is_empty());
    let expected_err = format!(
        "error: Destination already exists: {}. Use --force to overwrite.\n",
        default_dest.display()
    );
    assert_eq!(String::from_utf8_lossy(&merge2.stderr), expected_err);
    assert_eq!(read_file_bytes(&default_dest), orig_data);

    // Case 3: Forced replacement (--force)
    std::fs::write(&default_dest, b"modified corrupt destination bytes").unwrap();
    let merge3 = chunker_bin()
        .args(["merge", bundle_path.to_str().unwrap(), "--force"])
        .output()
        .unwrap();
    assert_eq!(merge3.status.code(), Some(0));
    assert!(merge3.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&merge3.stdout),
        format!("Merged file: {}\n", default_dest.display())
    );
    assert_eq!(read_file_bytes(&default_dest), orig_data);

    // Case 4: Existing directory as output
    let out_dir = temp.path().join("merge_target_dir");
    std::fs::create_dir_all(&out_dir).unwrap();
    let merge4 = chunker_bin()
        .args([
            "merge",
            bundle_path.to_str().unwrap(),
            "--output",
            out_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let target_dest = out_dir.join("test_input.bin");
    assert_eq!(merge4.status.code(), Some(0));
    assert!(merge4.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&merge4.stdout),
        format!("Merged file: {}\n", target_dest.display())
    );
    assert_eq!(read_file_bytes(&target_dest), orig_data);

    // Case 5: Exact file path as output
    let custom_dest = temp.path().join("custom_merged.bin");
    let merge5 = chunker_bin()
        .args([
            "merge",
            bundle_path.to_str().unwrap(),
            "--output",
            custom_dest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(merge5.status.code(), Some(0));
    assert!(merge5.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&merge5.stdout),
        format!("Merged file: {}\n", custom_dest.display())
    );
    assert_eq!(read_file_bytes(&custom_dest), orig_data);
}

#[test]
fn test_cli_verify_success_and_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("verify_input.bin");
    std::fs::write(&input_path, b"data to verify in bundle").unwrap();

    let split_res = chunker_bin()
        .args(["split", input_path.to_str().unwrap(), "--chunk-size", "1"])
        .output()
        .unwrap();
    assert_eq!(split_res.status.code(), Some(0));
    let bundle_path = temp.path().join("verify_input.bin.chunks");
    assert_eq!(
        String::from_utf8_lossy(&split_res.stdout),
        format!("Created bundle: {}\n", bundle_path.display())
    );
    assert!(split_res.stderr.is_empty());

    // Verify success
    let verify1 = chunker_bin()
        .args(["verify", bundle_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(verify1.status.code(), Some(0));
    assert!(verify1.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&verify1.stdout),
        format!("Verified bundle: {}\n", bundle_path.display())
    );

    // Corrupt a chunk
    let chunk_path = bundle_path.join("part-0000.chk");
    let mut chunk_bytes = read_file_bytes(&chunk_path);
    chunk_bytes[0] ^= 0xFF;
    std::fs::write(&chunk_path, chunk_bytes).unwrap();

    let verify2 = chunker_bin()
        .args(["verify", bundle_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(verify2.status.code(), Some(1));
    assert!(verify2.stdout.is_empty());
    let expected_hash = "80731cc3538e67bd638ae28b057b60b77fa555a09c78ad1b7cbd91de3b4275b8";
    let actual_hash = "78e99a9ad156aa4f33d42f7c782994e3bb4fa849a7d5e1fa7442e94603dff9df";
    let expected_diag = format!(
        "error: Hash mismatch for part-0000.chk: expected {expected_hash}, got {actual_hash}\n"
    );
    assert_eq!(String::from_utf8_lossy(&verify2.stderr), expected_diag);
}

#[test]
fn test_cli_bundle_path_is_regular_file_or_missing() {
    let temp = tempfile::tempdir().unwrap();
    let regular_file = temp.path().join("not_a_dir.bin");
    std::fs::write(&regular_file, b"i am a regular file").unwrap();

    let expected_dir_err = format!(
        "error: Invalid argument: Bundle path must be a directory: {}\n",
        regular_file.display()
    );

    // Merge with regular file
    let merge_reg = chunker_bin()
        .args(["merge", regular_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(merge_reg.status.code(), Some(2));
    assert!(merge_reg.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&merge_reg.stderr), expected_dir_err);

    // Verify with regular file
    let verify_reg = chunker_bin()
        .args(["verify", regular_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(verify_reg.status.code(), Some(2));
    assert!(verify_reg.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&verify_reg.stderr),
        expected_dir_err
    );

    // Missing bundle path
    let missing_bundle = temp.path().join("missing_bundle.chunks");
    let os_error = File::open(missing_bundle.join("manifest.json")).unwrap_err();
    let expected_io_err = format!("error: I/O error: {os_error}\n");

    let merge_miss = chunker_bin()
        .args(["merge", missing_bundle.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(merge_miss.status.code(), Some(1));
    assert!(merge_miss.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&merge_miss.stderr), expected_io_err);

    let verify_miss = chunker_bin()
        .args(["verify", missing_bundle.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(verify_miss.status.code(), Some(1));
    assert!(verify_miss.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&verify_miss.stderr),
        expected_io_err
    );
}

#[test]
fn test_cli_clap_failures_and_help() {
    // Bad flag
    let bad_flag = chunker_bin().arg("--bad-flag").output().unwrap();
    assert_eq!(bad_flag.status.code(), Some(2));
    assert!(bad_flag.stdout.is_empty());
    let bad_flag_stderr = String::from_utf8_lossy(&bad_flag.stderr);
    assert!(
        bad_flag_stderr.contains("--bad-flag"),
        "Expected stderr to contain '--bad-flag', got: {bad_flag_stderr}"
    );

    // Non-numeric size
    let non_num = chunker_bin()
        .args(["split", "input.bin", "--chunk-size", "abc"])
        .output()
        .unwrap();
    assert_eq!(non_num.status.code(), Some(2));
    assert!(non_num.stdout.is_empty());
    let non_num_err = "abc".parse::<u64>().unwrap_err().to_string();
    assert!(String::from_utf8_lossy(&non_num.stderr).contains(&non_num_err));

    // Size 0
    let size_zero = chunker_bin()
        .args(["split", "input.bin", "--chunk-size", "0"])
        .output()
        .unwrap();
    assert_eq!(size_zero.status.code(), Some(2));
    assert!(size_zero.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&size_zero.stderr).contains("chunk size must be at least 1 MiB")
    );

    // Size 17592186044416 (checked_mul overflow)
    let size_overflow = chunker_bin()
        .args(["split", "input.bin", "--chunk-size", "17592186044416"])
        .output()
        .unwrap();
    assert_eq!(size_overflow.status.code(), Some(2));
    assert!(size_overflow.stdout.is_empty());
    assert!(String::from_utf8_lossy(&size_overflow.stderr).contains("chunk size is too large"));

    // Size 18446744073709551616 (2^64, u64 parse error)
    let size_2_64 = chunker_bin()
        .args(["split", "input.bin", "--chunk-size", "18446744073709551616"])
        .output()
        .unwrap();
    assert_eq!(size_2_64.status.code(), Some(2));
    assert!(size_2_64.stdout.is_empty());
    let expected_u64_err = "18446744073709551616"
        .parse::<u64>()
        .unwrap_err()
        .to_string();
    let err_str = String::from_utf8_lossy(&size_2_64.stderr);
    assert!(err_str.contains(&expected_u64_err));
    assert!(!err_str.contains("chunk size is too large"));

    // --help succeeds (exit 0)
    let help = chunker_bin().arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
}

#[test]
fn test_cli_resume_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let sample_path = temp.path().join("sample.bin");

    // 1 MiB of zeros followed by one 0xA5 byte
    let mut data = vec![0u8; 1024 * 1024];
    data.push(0xA5);
    std::fs::write(&sample_path, &data).unwrap();

    let abs_sample_path = sample_path.canonicalize().unwrap();

    // Run 1: Split at chunk size 1 MiB
    let run1 = chunker_bin()
        .args([
            "split",
            abs_sample_path.to_str().unwrap(),
            "--chunk-size",
            "1",
        ])
        .output()
        .unwrap();

    let bundle_path = abs_sample_path.parent().unwrap().join(format!(
        "{}.chunks",
        abs_sample_path.file_name().unwrap().to_str().unwrap()
    ));

    assert_eq!(run1.status.code(), Some(0));
    assert!(run1.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&run1.stdout),
        format!("Created bundle: {}\n", bundle_path.display())
    );

    // Delete ONLY part-0001.chk, leaving the manifest
    let chunk1_path = bundle_path.join("part-0001.chk");
    assert!(chunk1_path.exists());
    std::fs::remove_file(&chunk1_path).unwrap();

    // Run 2: Identical command
    let run2 = chunker_bin()
        .args([
            "split",
            abs_sample_path.to_str().unwrap(),
            "--chunk-size",
            "1",
        ])
        .output()
        .unwrap();

    assert_eq!(run2.status.code(), Some(0));
    assert!(run2.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&run2.stdout),
        format!(
            "Created bundle: {}\nReused chunks: 1\n",
            bundle_path.display()
        )
    );

    // Read the restored chunk and assert it is exactly [0xA5]
    let restored_chunk_bytes = read_file_bytes(&chunk1_path);
    assert_eq!(restored_chunk_bytes, vec![0xA5]);
}

#[test]
fn test_cli_zero_byte_split() {
    let temp = tempfile::tempdir().unwrap();
    let empty_path = temp.path().join("empty.bin");
    std::fs::write(&empty_path, b"").unwrap();

    let output = chunker_bin()
        .args(["split", empty_path.to_str().unwrap(), "--chunk-size", "1"])
        .output()
        .unwrap();

    let bundle_path = temp.path().join("empty.bin.chunks");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("Created bundle: {}\n", bundle_path.display())
    );
}
