#[test]
fn test_fs4_available_space_compiles_and_runs() {
    let temp_dir = std::env::temp_dir();
    let space = fs4::available_space(&temp_dir);
    assert!(
        space.is_ok(),
        "fs4::available_space must succeed on temp directory"
    );
    assert!(space.unwrap() > 0, "Available space should be non-zero");
}

#[test]
fn test_default_constants_exposed() {
    assert_eq!(chunker_core::BUFFER_SIZE, 1024 * 1024);
    assert_eq!(chunker_core::DEFAULT_CHUNK_SIZE, 500 * 1024 * 1024);
}
