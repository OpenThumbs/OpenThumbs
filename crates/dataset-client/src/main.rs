use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dataset_client::{seg, Client, VersionSpec};
use dataset_core::{DatasetRef, Producer};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "ds", about = "Versioned dataset store client")]
struct Cli {
    #[arg(long, env = "DS_URL", default_value = "http://127.0.0.1:62541", global = true)]
    url: String,
    #[arg(long, env = "DS_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Exchange username/password (DS_PASSWORD) for an API token.
    Login {
        #[arg(long)]
        username: String,
        #[arg(long, env = "DS_PASSWORD", hide_env_values = true)]
        password: String,
    },
    #[command(subcommand)]
    Dataset(DatasetCmd),
    #[command(subcommand)]
    Version(VersionCmd),
    /// Resolve `dataset@ref` to an exact version, manifest hash and blob URLs.
    Resolve { target: String },
    /// Download `dataset@ref` into a directory (resumable, verified).
    Pull {
        target: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Create an immutable tag.
    Tag { dataset: String, name: String, version: String },
    /// Create or move a branch.
    Branch { dataset: String, name: String, version: String },
    /// Compare two versions (ids, tags, branches or `latest`).
    Diff { dataset: String, old: String, new: String },
    /// Show lineage around `dataset@ref`.
    Lineage {
        target: String,
        #[arg(long, default_value_t = 3)]
        depth: usize,
    },
    /// Record that `dataset@ref` was used by something (e.g. `mlflow://run/<id>`).
    Used { target: String, by: String },
    /// Garbage-collect unreferenced blobs (admin). Dry run unless --apply.
    Gc {
        #[arg(long)]
        apply: bool,
        #[arg(long, default_value_t = 24)]
        min_age_hours: u64,
    },
}

#[derive(Subcommand)]
enum DatasetCmd {
    List {
        #[arg(long)]
        search: Option<String>,
    },
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    Show { name: String },
}

#[derive(Subcommand)]
enum VersionCmd {
    /// Create a version from a local directory; only new blobs are uploaded.
    Create {
        dataset: String,
        #[arg(long)]
        from: PathBuf,
        /// Parent version (id, tag, branch).
        #[arg(long)]
        parent: Option<String>,
        /// Branch to point at the new version.
        #[arg(long)]
        branch: Option<String>,
        /// `type:id` (e.g. `pipeline:run-123`); bare ids get type `run`.
        #[arg(long)]
        producer: Option<String>,
        /// Input datasets (`name@ref`, pinned to exact versions) or URIs. Repeatable.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// Metadata `key=value` (value parsed as JSON when possible). Repeatable.
        #[arg(long = "meta")]
        meta: Vec<String>,
        #[arg(long)]
        schema_hash: Option<String>,
        /// Short description of what changed (stored as metadata.message).
        #[arg(short, long)]
        message: Option<String>,
    },
    List {
        dataset: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Show { target: String },
}

fn parse_target(s: &str) -> Result<DatasetRef> {
    DatasetRef::parse(s).with_context(|| format!("invalid target {s:?}; expected dataset[@ref]"))
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let c = Client::new(&cli.url, cli.token.clone());

    match cli.command {
        Command::Login { username, password } => {
            let token = c.login(&username, &password).await?;
            println!("export DS_TOKEN={token}");
        }
        Command::Dataset(DatasetCmd::List { search }) => {
            let q = search.map(|s| format!("?search={}", seg(&s))).unwrap_or_default();
            print(&c.get(&format!("/datasets{q}")).await?);
        }
        Command::Dataset(DatasetCmd::Create { name, description }) => {
            print(&c.post("/datasets", json!({ "name": name, "description": description })).await?);
        }
        Command::Dataset(DatasetCmd::Show { name }) => print(&c.get(&format!("/datasets/{}", seg(&name))).await?),
        Command::Version(VersionCmd::Create { dataset, from, parent, branch, producer, inputs, meta, schema_hash, message }) => {
            let mut metadata = serde_json::Map::new();
            if let Some(m) = message {
                metadata.insert("message".into(), Value::String(m));
            }
            for kv in meta {
                let (k, v) = kv.split_once('=').with_context(|| format!("--meta {kv:?}: expected key=value"))?;
                metadata.insert(k.into(), serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.into())));
            }
            let producer = producer.map(|p| match p.split_once(':') {
                Some((kind, id)) => Producer { kind: kind.into(), id: id.into() },
                None => Producer { kind: "run".into(), id: p },
            });
            let spec = VersionSpec { parent, branch, schema_hash, metadata: Value::Object(metadata), producer, inputs };
            print(&c.push_dir(&dataset, &from, spec).await?);
        }
        Command::Version(VersionCmd::List { dataset, limit }) => {
            print(&c.get(&format!("/datasets/{}/versions?limit={limit}", seg(&dataset))).await?);
        }
        Command::Version(VersionCmd::Show { target }) => {
            let t = parse_target(&target)?;
            print(&c.get(&format!("/datasets/{}/versions/{}", seg(&t.dataset), seg(&t.reference))).await?);
        }
        Command::Resolve { target } => {
            let t = parse_target(&target)?;
            print(&serde_json::to_value(c.resolve(&t.dataset, &t.reference).await?)?);
        }
        Command::Pull { target, to } => {
            let t = parse_target(&target)?;
            let r = c.pull(&t.dataset, &t.reference, &to).await?;
            print(&json!({ "dataset": r.dataset, "version": r.version, "manifest_hash": r.manifest_hash, "files": r.files.len(), "to": to }));
        }
        Command::Tag { dataset, name, version } => {
            print(&c.post(&format!("/datasets/{}/tags", seg(&dataset)), json!({ "name": name, "version": version })).await?);
        }
        Command::Branch { dataset, name, version } => {
            print(&c.post(&format!("/datasets/{}/branches", seg(&dataset)), json!({ "name": name, "version": version })).await?);
        }
        Command::Diff { dataset, old, new } => {
            print(&c.get(&format!("/compare/{}/{}/{}", seg(&dataset), seg(&old), seg(&new))).await?);
        }
        Command::Lineage { target, depth } => {
            let t = parse_target(&target)?;
            print(&c.get(&format!("/datasets/{}/versions/{}/lineage?depth={depth}", seg(&t.dataset), seg(&t.reference))).await?);
        }
        Command::Used { target, by } => {
            parse_target(&target)?;
            c.post("/lineage", json!({ "edges": [{ "from": target, "to": by, "kind": "USED_BY" }] })).await?;
            eprintln!("recorded {target} USED_BY {by}");
        }
        Command::Gc { apply, min_age_hours } => {
            print(&c.post("/admin/gc", json!({ "dry_run": !apply, "min_age_hours": min_age_hours })).await?);
        }
    }
    Ok(())
}
