//! Small shared helpers.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Streaming SHA-256 of a file, hex-encoded. Used for catalog integrity checks
/// and de-duplication (the same disk read twice should be recognisable).
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Streaming SHA-1 of a file, hex-encoded. Used to verify downloads against the
/// Internet Archive's per-file `sha1` metadata.
pub fn sha1_file(path: &Path) -> io::Result<String> {
    use sha1::Sha1;
    let mut file = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(40);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_known_content() {
        let mut path = std::env::temp_dir();
        path.push(format!("gwm-hash-test-{}.bin", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        let hash = sha256_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
