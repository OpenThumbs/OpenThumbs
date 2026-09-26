//! Core object model shared by server, client and UI.
//!
//! ```text
//! Dataset
//!   └─ Version (immutable, ULID id)
//!       ├─ Manifest  (path → sha256 blob, size)
//!       ├─ Metadata  (free-form JSON: rows, format, …)
//!       └─ Lineage   (typed edges between URIs)
//! ```

pub mod compare;
pub mod hash;
pub mod lineage;
pub mod manifest;
pub mod refs;

pub use compare::{diff, ChangedFile, StorageDiff};
pub use hash::{BlobHash, BlobHasher};
pub use lineage::{dataset_uri, EdgeKind, LineageEdge, Producer};
pub use manifest::{manifest_hash, validate_path, Manifest, ManifestFile};
pub use refs::{validate_name, DatasetRef, RefKind};

/// New sortable, globally unique version id.
pub fn new_version_id() -> String {
    ulid::Ulid::new().to_string()
}

/// UTC timestamp with a fixed-width format, so string comparison == time comparison
/// (timestamps are stored as TEXT to stay portable across SQLite and Postgres).
pub fn timestamp(t: time::OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

pub fn now() -> String {
    timestamp(time::OffsetDateTime::now_utc())
}
