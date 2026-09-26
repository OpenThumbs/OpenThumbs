use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
#[error("invalid blob hash {0:?}: expected sha256:<64 lowercase hex>")]
pub struct InvalidHash(pub String);

/// SHA-256 content address. Displays and serializes as `sha256:<hex>`;
/// parsing also accepts bare hex.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlobHash(String);

impl BlobHash {
    pub fn parse(s: &str) -> Result<Self, InvalidHash> {
        let hex = s.strip_prefix("sha256:").unwrap_or(s);
        if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(BlobHash(hex.to_string()))
        } else {
            Err(InvalidHash(s.to_string()))
        }
    }

    pub fn hex(&self) -> &str {
        &self.0
    }

    pub fn of(data: &[u8]) -> Self {
        let mut h = BlobHasher::new();
        h.update(data);
        h.finish()
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.0)
    }
}

impl TryFrom<String> for BlobHash {
    type Error = InvalidHash;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        BlobHash::parse(&s)
    }
}

impl From<BlobHash> for String {
    fn from(h: BlobHash) -> String {
        h.to_string()
    }
}

impl std::str::FromStr for BlobHash {
    type Err = InvalidHash;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        BlobHash::parse(s)
    }
}

/// Incremental hasher that also counts bytes.
#[derive(Default)]
pub struct BlobHasher {
    inner: Sha256,
    len: u64,
}

impl BlobHasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
        self.len += data.len() as u64;
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn finish(self) -> BlobHash {
        BlobHash(format!("{:x}", self.inner.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        let h = BlobHash::of(b"hello");
        assert_eq!(h.hex(), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        assert_eq!(BlobHash::parse(&h.to_string()).unwrap(), h);
        assert_eq!(BlobHash::parse(h.hex()).unwrap(), h);
        assert!(BlobHash::parse("sha256:ABC").is_err());
    }
}
