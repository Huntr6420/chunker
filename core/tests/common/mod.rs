#![allow(dead_code)]

// Each integration-test file compiles this module separately and may use only some helpers.

use tempfile::TempDir;

/// Generates `len` deterministic pseudo-random bytes from `seed` using a hand-written
/// SplitMix64 algorithm.
///
/// Unlike plain xorshift, SplitMix64 does not get stuck when `seed == 0` because it uses
/// a Weyl sequence increment (`+ 0x9e3779b97f4a7c15`).
pub fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut bytes = Vec::with_capacity(len);

    while bytes.len() < len {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;

        let chunk = z.to_le_bytes();
        let needed = (len - bytes.len()).min(8);
        bytes.extend_from_slice(&chunk[..needed]);
    }

    bytes
}

/// Creates a new temporary directory for integration tests.
///
/// Callers must retain the returned [`TempDir`] guard for the entire duration of the test.
pub fn make_temp_dir() -> TempDir {
    tempfile::tempdir().expect("failed to create temporary test directory")
}
