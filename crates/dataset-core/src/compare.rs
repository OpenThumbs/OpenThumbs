use std::collections::BTreeMap;

use serde::Serialize;

use crate::hash::BlobHash;
use crate::manifest::ManifestFile;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub old_blob: BlobHash,
    pub new_blob: BlobHash,
    pub old_size: u64,
    pub new_size: u64,
}

/// Storage-level diff between two manifests.
#[derive(Debug, Default, Serialize)]
pub struct StorageDiff {
    pub added: Vec<ManifestFile>,
    pub removed: Vec<ManifestFile>,
    pub changed: Vec<ChangedFile>,
    pub unchanged: u64,
    pub old_bytes: u64,
    pub new_bytes: u64,
    /// Bytes of `new` whose blobs are absent from `old` (what a transfer must move).
    pub new_blob_bytes: u64,
}

pub fn diff(old: &[ManifestFile], new: &[ManifestFile]) -> StorageDiff {
    let old_by_path: BTreeMap<&str, &ManifestFile> = old.iter().map(|f| (f.path.as_str(), f)).collect();
    let new_by_path: BTreeMap<&str, &ManifestFile> = new.iter().map(|f| (f.path.as_str(), f)).collect();
    let old_blobs: std::collections::HashSet<&BlobHash> = old.iter().map(|f| &f.blob).collect();

    let mut d = StorageDiff {
        old_bytes: old.iter().map(|f| f.size).sum(),
        new_bytes: new.iter().map(|f| f.size).sum(),
        ..Default::default()
    };
    let mut counted = std::collections::HashSet::new();
    for f in new {
        if !old_blobs.contains(&f.blob) && counted.insert(&f.blob) {
            d.new_blob_bytes += f.size;
        }
    }
    for (path, n) in &new_by_path {
        match old_by_path.get(path) {
            None => d.added.push((*n).clone()),
            Some(o) if o.blob != n.blob => d.changed.push(ChangedFile {
                path: path.to_string(),
                old_blob: o.blob.clone(),
                new_blob: n.blob.clone(),
                old_size: o.size,
                new_size: n.size,
            }),
            Some(_) => d.unchanged += 1,
        }
    }
    for (path, o) in &old_by_path {
        if !new_by_path.contains_key(path) {
            d.removed.push((*o).clone());
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile { path: path.into(), blob: BlobHash::of(data), size: data.len() as u64 }
    }

    #[test]
    fn diffs() {
        let v1 = vec![f("a", b"A"), f("b", b"B"), f("c", b"C")];
        let v2 = vec![f("a", b"A"), f("b", b"BB"), f("d", b"DDD")];
        let d = diff(&v1, &v2);
        assert_eq!(d.unchanged, 1);
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.new_blob_bytes, 5);
    }
}
