//!
//! Only the streamed SHA-256 case is ported: the OS file-lock helpers
//! (`open_lock`, `lock_exclusive`, `lock_shared`, `unlock`, `fsync_directory`) have no Rust
//! counterpart yet, so the remaining reference cases are listed as deferred in the commit report.

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use spiderhead_core::filesystem::file_sha256;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-core-ported-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn file_sha256_streams_across_chunk_boundaries() {
    let mut payload = vec![b'a'; 1 << 20];
    payload.extend_from_slice(b"tail");
    let dir = scratch("sha-chunks");
    let path = dir.join("payload.bin");
    fs::write(&path, &payload).unwrap();

    let expected: String = Sha256::digest(&payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(file_sha256(&path).unwrap(), expected);
    assert!(file_sha256(&dir.join("missing.bin")).is_err());

    fs::remove_dir_all(&dir).unwrap();
}
