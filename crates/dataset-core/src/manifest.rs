use serde::{Deserialize, Serialize};

use crate::hash::{BlobHash, BlobHasher};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub blob: BlobHash,
    pub size: u64,
}

/// Full description of an immutable dataset version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub dataset: String,
    pub version: String,
    pub parent: Option<String>,
    pub manifest_hash: BlobHash,
    pub files: Vec<ManifestFile>,
    pub schema_hash: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Content identity of a file set: sha256 over `path \0 hash \0 size \n`
/// lines sorted by path. Two versions with identical bytes share this hash,
/// regardless of version id, parent or metadata.
pub fn manifest_hash(files: &[ManifestFile]) -> BlobHash {
    let mut sorted: Vec<&ManifestFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = BlobHasher::new();
    for f in sorted {
        h.update(f.path.as_bytes());
        h.update(b"\0");
        h.update(f.blob.hex().as_bytes());
        h.update(b"\0");
        h.update(f.size.to_string().as_bytes());
        h.update(b"\n");
    }
    h.finish()
}

/// Relative `/`-separated path without `.`/`..`/empty segments.
pub fn validate_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\\')
        && !path.contains('\0')
        && path.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile { path: path.into(), blob: BlobHash::of(data), size: data.len() as u64 }
    }

    #[test]
    fn hash_is_order_independent() {
        let a = vec![f("a", b"1"), f("b", b"2")];
        let b = vec![f("b", b"2"), f("a", b"1")];
        assert_eq!(manifest_hash(&a), manifest_hash(&b));
        assert_ne!(manifest_hash(&a), manifest_hash(&[f("a", b"1")]));
    }

    #[test]
    fn paths() {
        assert!(validate_path("part-000.parquet"));
        assert!(validate_path("year=2026/part-000.parquet"));
        for bad in ["", "/abs", "a//b", "../x", "a/./b", "a\\b"] {
            assert!(!validate_path(bad), "{bad}");
        }
    }
}
