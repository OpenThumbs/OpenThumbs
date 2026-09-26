use std::io::SeekFrom;
use std::ops::Range;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use dataset_core::{BlobHash, BlobHasher};
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::{BlobRef, BlobStore, ByteStream, Result, StorageError};

const READ_BUF: usize = 1 << 20;

/// Filesystem CAS: `<root>/blobs/sha256/ab/cd/<hex>`.
/// Writes go to `<root>/tmp` then are renamed into place (atomic, same filesystem).
pub struct LocalBlobStore {
    root: PathBuf,
}

impl LocalBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("blobs/sha256"))?;
        std::fs::create_dir_all(root.join("tmp"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        let h = hash.hex();
        self.root.join("blobs/sha256").join(&h[..2]).join(&h[2..4]).join(h)
    }

    fn tmp_path(&self) -> PathBuf {
        self.root.join("tmp").join(ulid::Ulid::new().to_string())
    }

    async fn write_tmp(&self, mut data: ByteStream, tmp: &Path) -> Result<(BlobHash, u64)> {
        let mut file = tokio::fs::File::create(tmp).await?;
        let mut hasher = BlobHasher::new();
        while let Some(chunk) = data.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        let size = hasher.len();
        Ok((hasher.finish(), size))
    }
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    async fn size(&self, hash: &BlobHash) -> Result<Option<u64>> {
        match tokio::fs::metadata(self.blob_path(hash)).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn put(&self, data: ByteStream, expected: Option<&BlobHash>) -> Result<BlobRef> {
        let tmp = self.tmp_path();
        let (hash, size) = match self.write_tmp(data, &tmp).await {
            Ok(v) => v,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        };
        if let Some(expected) = expected {
            if *expected != hash {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(StorageError::HashMismatch { expected: expected.clone(), actual: hash });
            }
        }
        let dest = self.blob_path(&hash);
        if tokio::fs::try_exists(&dest).await? {
            tokio::fs::remove_file(&tmp).await?;
            return Ok(BlobRef { hash, size, created: false });
        }
        tokio::fs::create_dir_all(dest.parent().expect("blob path has parent")).await?;
        tokio::fs::rename(&tmp, &dest).await?;
        Ok(BlobRef { hash, size, created: true })
    }

    async fn get(&self, hash: &BlobHash, range: Option<Range<u64>>) -> Result<ByteStream> {
        let mut file = match tokio::fs::File::open(self.blob_path(hash)).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StorageError::NotFound),
            Err(e) => return Err(e.into()),
        };
        match range {
            None => Ok(Box::pin(ReaderStream::with_capacity(file, READ_BUF))),
            Some(r) => {
                let len = file.metadata().await?.len();
                if r.start > r.end || r.end > len {
                    return Err(StorageError::InvalidRange);
                }
                file.seek(SeekFrom::Start(r.start)).await?;
                Ok(Box::pin(ReaderStream::with_capacity(file.take(r.end - r.start), READ_BUF)))
            }
        }
    }

    async fn delete(&self, hash: &BlobHash) -> Result<()> {
        match tokio::fs::remove_file(self.blob_path(hash)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    pub fn stream(data: &'static [u8]) -> ByteStream {
        Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from_static(data))]))
    }

    async fn collect(mut s: ByteStream) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = s.next().await {
            out.extend_from_slice(&c.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn put_get_dedup_range() {
        let dir = std::env::temp_dir().join(format!("ds-test-{}", ulid::Ulid::new()));
        let store = LocalBlobStore::new(&dir).unwrap();
        let a = store.put(stream(b"hello world"), None).await.unwrap();
        assert!(a.created);
        let b = store.put(stream(b"hello world"), Some(&a.hash)).await.unwrap();
        assert!(!b.created);
        assert_eq!(collect(store.get(&a.hash, None).await.unwrap()).await, b"hello world");
        assert_eq!(collect(store.get(&a.hash, Some(6..11)).await.unwrap()).await, b"world");
        let wrong = BlobHash::of(b"nope");
        assert!(matches!(store.put(stream(b"x"), Some(&wrong)).await, Err(StorageError::HashMismatch { .. })));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
