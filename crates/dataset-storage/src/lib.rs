//! Blob storage. Blobs are immutable and addressed by sha256; the store is
//! the only place bytes live. Versions are manifests pointing at blobs, so
//! unchanged files are never copied.

mod local;
mod uploads;

use std::ops::Range;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use dataset_core::BlobHash;
use futures_util::Stream;

pub use local::LocalBlobStore;
pub use uploads::{PartInfo, UploadInfo, UploadManager};

pub type ByteStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("blob not found")]
    NotFound,
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: BlobHash, actual: BlobHash },
    #[error("size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("invalid range")]
    InvalidRange,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BlobRef {
    pub hash: BlobHash,
    pub size: u64,
    /// false when the blob already existed (deduplicated).
    pub created: bool,
}

#[async_trait]
pub trait BlobStore: Send + Sync + 'static {
    async fn exists(&self, hash: &BlobHash) -> Result<bool> {
        Ok(self.size(hash).await?.is_some())
    }

    async fn size(&self, hash: &BlobHash) -> Result<Option<u64>>;

    /// Stream data into the store. The hash is always computed and verified
    /// against `expected` (when given) before the blob becomes visible.
    async fn put(&self, data: ByteStream, expected: Option<&BlobHash>) -> Result<BlobRef>;

    /// Read a blob, optionally a byte range (end exclusive).
    async fn get(&self, hash: &BlobHash, range: Option<Range<u64>>) -> Result<ByteStream>;

    async fn delete(&self, hash: &BlobHash) -> Result<()>;
}
