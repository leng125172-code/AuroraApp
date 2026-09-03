//! RFC 8785 canonicalization and SHA-256 digest helpers.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{BuildError, BuildResult};

const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_JSON_READ_BYTES: u64 = 16 * 1024 * 1024 + 1;

/// Calculate a prefixed SHA-256 digest for arbitrary bytes.
pub(crate) fn sha256(bytes: &[u8]) -> String {
    let mut digest = String::with_capacity(71);
    digest.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        let _format_result = write!(digest, "{byte:02x}");
    }
    digest
}

/// Read JSON, canonicalize it according to RFC 8785, then hash it.
pub(crate) fn canonical_json_digest(path: &Path) -> BuildResult<String> {
    let value = read_json(path)?;
    let canonical = serde_jcs::to_vec(&value).map_err(|error| {
        BuildError::Validation(format!(
            "canonicalize JSON `{}` according to RFC 8785: {error}",
            path.display()
        ))
    })?;
    Ok(sha256(&canonical))
}

/// Read and parse one JSON document with its path in any error.
pub(crate) fn read_json(path: &Path) -> BuildResult<Value> {
    let file = File::open(path).map_err(|source| BuildError::Io {
        operation: "open",
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_JSON_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|source| BuildError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(BuildError::Validation(format!(
            "JSON `{}` exceeds the 16 MiB input limit",
            path.display()
        )));
    }
    serde_json::from_slice(&bytes).map_err(|source| BuildError::Json {
        path: path.to_path_buf(),
        source,
    })
}

/// Write stable, pretty JSON followed by one newline.
pub(crate) fn write_json(path: &Path, value: &Value) -> BuildResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| BuildError::Io {
            operation: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        BuildError::Validation(format!("serialize JSON `{}`: {error}", path.display()))
    })?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|source| BuildError::Io {
        operation: "write",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::sha256;

    #[test]
    fn sha256_uses_lowercase_prefixed_hex() {
        assert_eq!(
            sha256(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
