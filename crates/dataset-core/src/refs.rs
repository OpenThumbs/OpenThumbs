use serde::{Deserialize, Serialize};

/// Named pointers to versions. Tags are immutable aliases; branches move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Tag,
    Branch,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Tag => "tag",
            RefKind::Branch => "branch",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tag" => Some(RefKind::Tag),
            "branch" => Some(RefKind::Branch),
            _ => None,
        }
    }
}

/// `dataset[@ref]`, where ref is a version id, tag, branch or `latest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetRef {
    pub dataset: String,
    pub reference: String,
}

impl DatasetRef {
    pub fn parse(s: &str) -> Option<Self> {
        let (dataset, reference) = match s.split_once('@') {
            Some((d, r)) => (d, r),
            None => (s, "latest"),
        };
        (validate_name(dataset) && validate_name(reference))
            .then(|| DatasetRef { dataset: dataset.into(), reference: reference.into() })
    }
}

impl std::fmt::Display for DatasetRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.dataset, self.reference)
    }
}

/// Names for datasets, tags and branches. `/` is allowed for namespacing (`training/latest`).
pub fn validate_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with(['.', '-', '/'])
        && !s.ends_with('/')
        && !s.contains("//")
        && !s.contains("..")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refs() {
        let r = DatasetRef::parse("multicredit_v2@training/latest").unwrap();
        assert_eq!(r.dataset, "multicredit_v2");
        assert_eq!(r.reference, "training/latest");
        assert_eq!(DatasetRef::parse("x").unwrap().reference, "latest");
        assert!(DatasetRef::parse("../x").is_none());
    }
}
