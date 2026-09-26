//! Resumable multipart uploads staged on local disk.
//!
//! `create` → `put_part` (any order, retry-safe, parallel) → `status` (to resume)
//! → `complete` (concatenate parts in order, hash, verify, store).

use std::path::{Path, PathBuf};

use dataset_core::{BlobHash, BlobHasher};
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::{BlobRef, BlobStore, ByteStream, Result, StorageError};

pub const MAX_PARTS: u32 = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadInfo {
    pub id: String,
    pub expected_hash: Option<BlobHash>,
    pub expected_size: Option<u64>,
    pub created_at: String,
    #[serde(default)]
    pub parts: Vec<PartInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PartInfo {
    pub number: u32,
    pub size: u64,
}

pub struct UploadManager {
    root: PathBuf,
}

impl UploadManager {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn dir(&self, id: &str) -> Result<PathBuf> {
        // ULIDs only: prevents path traversal through the id.
        ulid::Ulid::from_string(id).map_err(|_| StorageError::NotFound)?;
        Ok(self.root.join(id))
    }

    fn part_path(dir: &Path, n: u32) -> PathBuf {
        dir.join(format!("part-{n:05}"))
    }

    pub async fn create(&self, expected_hash: Option<BlobHash>, expected_size: Option<u64>) -> Result<UploadInfo> {
        let id = ulid::Ulid::new().to_string();
        let dir = self.root.join(&id);
        tokio::fs::create_dir_all(&dir).await?;
        let info = UploadInfo { id, expected_hash, expected_size, created_at: dataset_core::now(), parts: vec![] };
        tokio::fs::write(dir.join("upload.json"), serde_json::to_vec(&info).expect("serializable")).await?;
        Ok(info)
    }

    pub async fn status(&self, id: &str) -> Result<UploadInfo> {
        let dir = self.dir(id)?;
        let raw = match tokio::fs::read(dir.join("upload.json")).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StorageError::NotFound),
            Err(e) => return Err(e.into()),
        };
        let mut info: UploadInfo = serde_json::from_slice(&raw).map_err(|e| StorageError::Invalid(e.to_string()))?;
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(e) = entries.next_entry().await? {
            let name = e.file_name();
            let Some(n) = name.to_str().and_then(|s| s.strip_prefix("part-")).and_then(|s| s.parse().ok()) else {
                continue;
            };
            info.parts.push(PartInfo { number: n, size: e.metadata().await?.len() });
        }
        info.parts.sort_by_key(|p| p.number);
        Ok(info)
    }

    /// Store one part (1-based). Re-uploading a part replaces it atomically.
    pub async fn put_part(&self, id: &str, number: u32, mut data: ByteStream) -> Result<PartInfo> {
        if number == 0 || number > MAX_PARTS {
            return Err(StorageError::Invalid(format!("part number must be 1..={MAX_PARTS}")));
        }
        let dir = self.dir(id)?;
        if !tokio::fs::try_exists(dir.join("upload.json")).await? {
            return Err(StorageError::NotFound);
        }
        let tmp = dir.join(format!(".tmp-{number}-{}", ulid::Ulid::new()));
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut size = 0u64;
        let res: Result<()> = async {
            while let Some(chunk) = data.next().await {
                let chunk = chunk?;
                size += chunk.len() as u64;
                file.write_all(&chunk).await?;
            }
            file.sync_all().await?;
            Ok(())
        }
        .await;
        if let Err(e) = res {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
        tokio::fs::rename(&tmp, Self::part_path(&dir, number)).await?;
        Ok(PartInfo { number, size })
    }

    /// Assemble parts 1..=N (must be contiguous) into a blob and delete the staging dir.
    pub async fn complete(&self, id: &str, store: &dyn BlobStore) -> Result<BlobRef> {
        let info = self.status(id).await?;
        if info.parts.is_empty() {
            return Err(StorageError::Invalid("upload has no parts".into()));
        }
        for (i, p) in info.parts.iter().enumerate() {
            if p.number != i as u32 + 1 {
                return Err(StorageError::Invalid(format!("missing part {}", i + 1)));
            }
        }
        let total: u64 = info.parts.iter().map(|p| p.size).sum();
        if let Some(expected) = info.expected_size {
            if expected != total {
                return Err(StorageError::SizeMismatch { expected, actual: total });
            }
        }
        let dir = self.dir(id)?;

        // Fast path: already stored (e.g. a retried `complete`); just verify the parts hash.
        if let Some(h) = &info.expected_hash {
            if store.exists(h).await? {
                let actual = hash_parts(&dir, info.parts.len() as u32).await?;
                if actual != *h {
                    return Err(StorageError::HashMismatch { expected: h.clone(), actual });
                }
                tokio::fs::remove_dir_all(&dir).await?;
                return Ok(BlobRef { hash: h.clone(), size: total, created: false });
            }
        }

        let stream = parts_stream(dir.clone(), info.parts.len() as u32);
        let blob = store.put(stream, info.expected_hash.as_ref()).await?;
        tokio::fs::remove_dir_all(&dir).await?;
        Ok(blob)
    }

    pub async fn abort(&self, id: &str) -> Result<()> {
        let dir = self.dir(id)?;
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StorageError::NotFound),
            Err(e) => Err(e.into()),
        }
    }
}

fn parts_stream(dir: PathBuf, count: u32) -> ByteStream {
    let s = futures_util::stream::iter(1..=count)
        .then(move |n| {
            let path = UploadManager::part_path(&dir, n);
            async move { tokio::fs::File::open(path).await.map(|f| ReaderStream::with_capacity(f, 1 << 20)) }
        })
        .try_flatten();
    Box::pin(s)
}

async fn hash_parts(dir: &Path, count: u32) -> Result<BlobHash> {
    let mut s = parts_stream(dir.to_path_buf(), count);
    let mut h = BlobHasher::new();
    while let Some(c) = s.next().await {
        h.update(&c?);
    }
    Ok(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalBlobStore;
    use bytes::Bytes;

    fn stream(data: &'static [u8]) -> ByteStream {
        Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from_static(data))]))
    }

    #[tokio::test]
    async fn multipart_out_of_order() {
        let dir = std::env::temp_dir().join(format!("ds-up-{}", ulid::Ulid::new()));
        let store = LocalBlobStore::new(dir.join("store")).unwrap();
        let ups = UploadManager::new(dir.join("uploads")).unwrap();
        let expected = BlobHash::of(b"aaabbbcc");
        let up = ups.create(Some(expected.clone()), Some(8)).await.unwrap();
        ups.put_part(&up.id, 3, stream(b"cc")).await.unwrap();
        ups.put_part(&up.id, 1, stream(b"aaa")).await.unwrap();
        assert!(ups.complete(&up.id, &store).await.is_err()); // part 2 missing
        ups.put_part(&up.id, 2, stream(b"bbb")).await.unwrap();
        assert_eq!(ups.status(&up.id).await.unwrap().parts.len(), 3);
        let blob = ups.complete(&up.id, &store).await.unwrap();
        assert_eq!(blob.hash, expected);
        assert!(ups.status(&up.id).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
