use serde::{Deserialize, Serialize};

/// Typed lineage relations. Edges run `from → to`:
/// `input INPUT_OF output`, `output DERIVED_FROM input`, `run GENERATED_BY`… etc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeKind {
    InputOf,
    DerivedFrom,
    MergedFrom,
    GeneratedBy,
    UsedBy,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::InputOf => "INPUT_OF",
            EdgeKind::DerivedFrom => "DERIVED_FROM",
            EdgeKind::MergedFrom => "MERGED_FROM",
            EdgeKind::GeneratedBy => "GENERATED_BY",
            EdgeKind::UsedBy => "USED_BY",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

/// What produced an output (pipeline run, notebook, MLflow run, …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Producer {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

/// Edge between two URIs: `dataset://name/version`, `mlflow://run/<id>`, `pipeline://run/<id>`…
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<Producer>,
}

pub fn dataset_uri(dataset: &str, version: &str) -> String {
    format!("dataset://{dataset}/{version}")
}
