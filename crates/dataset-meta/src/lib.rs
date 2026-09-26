//! Metadata store. One implementation over sqlx's `Any` driver serves both
//! deployment modes with identical behavior:
//!
//! * workstation: `sqlite://datasets.db?mode=rwc`
//! * team server: `postgres://user:pass@host/db`
//!
//! Blob bytes never live here; only hashes and sizes.

use std::collections::{BTreeSet, HashSet, VecDeque};

use dataset_core::{dataset_uri, BlobHash, EdgeKind, LineageEdge, ManifestFile, Producer, RefKind};
use serde::Serialize;
use sqlx::any::{install_default_drivers, AnyPoolOptions};
use sqlx::{AnyPool, Executor, FromRow};

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{} blobs are not uploaded yet", .0.len())]
    MissingBlobs(Vec<BlobHash>),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, MetaError>;

#[derive(Clone, Debug, FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Dataset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub owner: String,
    pub created_at: String,
}

#[derive(Clone, Debug, FromRow)]
pub struct DatasetSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub owner: String,
    pub created_at: String,
    pub version_count: i64,
    pub latest_id: Option<String>,
    pub latest_created_at: Option<String>,
    pub latest_size: Option<i64>,
    pub latest_files: Option<i64>,
    pub latest_metadata: Option<String>,
    pub latest_by: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Version {
    pub id: String,
    pub dataset: String,
    pub parent: Option<String>,
    pub manifest_hash: BlobHash,
    pub schema_hash: Option<String>,
    pub metadata: serde_json::Value,
    pub file_count: u64,
    pub total_size: u64,
    pub producer: Option<Producer>,
    pub created_by: String,
    pub created_at: String,
}

#[derive(FromRow)]
struct VersionRow {
    id: String,
    dataset: String,
    parent_id: Option<String>,
    manifest_hash: String,
    schema_hash: Option<String>,
    metadata: String,
    file_count: i64,
    total_size: i64,
    producer: Option<String>,
    created_by: String,
    created_at: String,
}

impl TryFrom<VersionRow> for Version {
    type Error = MetaError;
    fn try_from(r: VersionRow) -> Result<Self> {
        Ok(Version {
            id: r.id,
            dataset: r.dataset,
            parent: r.parent_id,
            manifest_hash: BlobHash::parse(&r.manifest_hash).map_err(|e| MetaError::Invalid(e.to_string()))?,
            schema_hash: r.schema_hash,
            metadata: serde_json::from_str(&r.metadata).unwrap_or(serde_json::Value::Null),
            file_count: r.file_count as u64,
            total_size: r.total_size as u64,
            producer: r.producer.and_then(|p| serde_json::from_str(&p).ok()),
            created_by: r.created_by,
            created_at: r.created_at,
        })
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RefEntry {
    pub name: String,
    pub kind: String,
    pub version_id: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LineageGraph {
    pub root: String,
    pub nodes: Vec<String>,
    pub edges: Vec<LineageEdge>,
}

pub struct NewVersion {
    pub parent: Option<String>,
    pub files: Vec<ManifestFile>,
    pub schema_hash: Option<String>,
    pub metadata: serde_json::Value,
    pub producer: Option<Producer>,
    /// URIs of inputs (typically `dataset://name/version`); recorded as INPUT_OF edges.
    pub inputs: Vec<String>,
    /// Move this branch to the new version.
    pub branch: Option<String>,
    pub created_by: String,
}

const VERSION_COLS: &str = "v.id, d.name AS dataset, v.parent_id, v.manifest_hash, v.schema_hash, v.metadata, \
     v.file_count, v.total_size, v.producer, v.created_by, v.created_at";

/// Rows per multi-row INSERT (4 params each; well under SQLite/Postgres limits).
const INSERT_CHUNK: usize = 200;
/// Params per `IN (...)` query.
const IN_CHUNK: usize = 500;

fn placeholders(start: usize, count: usize) -> String {
    (start..start + count).map(|i| format!("${i}")).collect::<Vec<_>>().join(", ")
}

#[derive(Clone)]
pub struct MetaStore {
    pool: AnyPool,
}

impl MetaStore {
    pub async fn connect(url: &str) -> Result<Self> {
        install_default_drivers();
        let sqlite = url.starts_with("sqlite:");
        let pool = AnyPoolOptions::new()
            .max_connections(if sqlite { 4 } else { 16 })
            .after_connect(move |conn, _| {
                Box::pin(async move {
                    if sqlite {
                        conn.execute("PRAGMA journal_mode = WAL").await?;
                        conn.execute("PRAGMA busy_timeout = 10000").await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await.map_err(|e| MetaError::Db(e.into()))?;
        Ok(Self { pool })
    }

    // ---------------- users & tokens ----------------

    pub async fn user_by_name(&self, username: &str) -> Result<Option<User>> {
        Ok(sqlx::query_as("SELECT id, username, password_hash FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn create_user(&self, username: &str, password_hash: &str) -> Result<()> {
        if self.user_by_name(username).await?.is_some() {
            return Err(MetaError::Conflict(format!("user {username} exists")));
        }
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES ($1, $2, $3, $4)")
            .bind(ulid::Ulid::new().to_string())
            .bind(username)
            .bind(password_hash)
            .bind(dataset_core::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn user_by_token_hash(&self, token_hash: &str) -> Result<Option<User>> {
        Ok(sqlx::query_as(
            "SELECT u.id, u.username, u.password_hash FROM users u JOIN tokens t ON t.user_id = u.id WHERE t.token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn insert_token(&self, user_id: &str, name: &str, token_hash: &str) -> Result<()> {
        sqlx::query("INSERT INTO tokens (id, user_id, name, token_hash, created_at) VALUES ($1, $2, $3, $4, $5)")
            .bind(ulid::Ulid::new().to_string())
            .bind(user_id)
            .bind(name)
            .bind(token_hash)
            .bind(dataset_core::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_tokens(&self, user_id: &str) -> Result<Vec<TokenInfo>> {
        Ok(sqlx::query_as("SELECT id, name, created_at FROM tokens WHERE user_id = $1 ORDER BY created_at")
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn delete_token(&self, user_id: &str, token_id: &str) -> Result<()> {
        let r = sqlx::query("DELETE FROM tokens WHERE id = $1 AND user_id = $2")
            .bind(token_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(MetaError::NotFound);
        }
        Ok(())
    }

    // ---------------- datasets ----------------

    pub async fn create_dataset(&self, name: &str, description: &str, owner: &str) -> Result<Dataset> {
        if self.get_dataset(name).await.is_ok() {
            return Err(MetaError::Conflict(format!("dataset {name} exists")));
        }
        let d = Dataset {
            id: ulid::Ulid::new().to_string(),
            name: name.into(),
            description: description.into(),
            owner: owner.into(),
            created_at: dataset_core::now(),
        };
        sqlx::query("INSERT INTO datasets (id, name, description, owner, created_at) VALUES ($1, $2, $3, $4, $5)")
            .bind(&d.id)
            .bind(&d.name)
            .bind(&d.description)
            .bind(&d.owner)
            .bind(&d.created_at)
            .execute(&self.pool)
            .await?;
        Ok(d)
    }

    pub async fn list_datasets(&self, search: Option<&str>) -> Result<Vec<Dataset>> {
        Ok(sqlx::query_as(
            "SELECT id, name, description, owner, created_at FROM datasets WHERE LOWER(name) LIKE $1 ORDER BY name",
        )
        .bind(format!("%{}%", search.unwrap_or("").to_lowercase()))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Datasets with their version count and latest-version summary (one query).
    pub async fn list_dataset_summaries(&self, search: Option<&str>) -> Result<Vec<DatasetSummary>> {
        Ok(sqlx::query_as(
            "SELECT d.id, d.name, d.description, d.owner, d.created_at, \
             (SELECT COUNT(*) FROM dataset_versions c WHERE c.dataset_id = d.id) AS version_count, \
             v.id AS latest_id, v.created_at AS latest_created_at, v.total_size AS latest_size, \
             v.file_count AS latest_files, v.metadata AS latest_metadata, v.created_by AS latest_by \
             FROM datasets d LEFT JOIN dataset_versions v \
             ON v.id = (SELECT MAX(x.id) FROM dataset_versions x WHERE x.dataset_id = d.id) \
             WHERE LOWER(d.name) LIKE $1 ORDER BY d.name",
        )
        .bind(format!("%{}%", search.unwrap_or("").to_lowercase()))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn delete_token_by_hash(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM tokens WHERE token_hash = $1").bind(token_hash).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_dataset(&self, name: &str) -> Result<Dataset> {
        sqlx::query_as("SELECT id, name, description, owner, created_at FROM datasets WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(MetaError::NotFound)
    }

    // ---------------- blobs ----------------

    /// Register a stored blob (idempotent) and mark it as recently seen.
    pub async fn record_blob(&self, hash: &BlobHash, size: u64) -> Result<()> {
        let now = dataset_core::now();
        sqlx::query(
            "INSERT INTO blobs (hash, size, created_at, last_seen_at) VALUES ($1, $2, $3, $3) \
             ON CONFLICT (hash) DO UPDATE SET last_seen_at = excluded.last_seen_at",
        )
        .bind(hash.hex())
        .bind(size as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Return the hashes not yet stored. Found blobs are marked seen, which
    /// protects them from GC between this check and version creation.
    pub async fn missing_blobs(&self, hashes: &[BlobHash]) -> Result<Vec<BlobHash>> {
        let wanted: BTreeSet<&BlobHash> = hashes.iter().collect();
        let wanted: Vec<&BlobHash> = wanted.into_iter().collect();
        let mut found = HashSet::new();
        let now = dataset_core::now();
        for chunk in wanted.chunks(IN_CHUNK) {
            let sql = format!("SELECT hash FROM blobs WHERE hash IN ({})", placeholders(1, chunk.len()));
            let mut q = sqlx::query_scalar::<_, String>(&sql);
            for h in chunk {
                q = q.bind(h.hex());
            }
            found.extend(q.fetch_all(&self.pool).await?);

            let ph = placeholders(2, chunk.len());
            let sql = format!("UPDATE blobs SET last_seen_at = $1 WHERE hash IN ({ph})");
            let mut q = sqlx::query(&sql).bind(&now);
            for h in chunk {
                q = q.bind(h.hex());
            }
            q.execute(&self.pool).await?;
        }
        Ok(wanted.into_iter().filter(|h| !found.contains(h.hex())).cloned().collect())
    }

    pub async fn blob_size(&self, hash: &BlobHash) -> Result<Option<u64>> {
        let size: Option<i64> = sqlx::query_scalar("SELECT size FROM blobs WHERE hash = $1")
            .bind(hash.hex())
            .fetch_optional(&self.pool)
            .await?;
        Ok(size.map(|s| s as u64))
    }

    /// Blobs referenced by no version and not seen since `cutoff`.
    pub async fn gc_candidates(&self, cutoff: &str) -> Result<Vec<(BlobHash, u64)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT b.hash, b.size FROM blobs b WHERE b.last_seen_at < $1 \
             AND NOT EXISTS (SELECT 1 FROM version_files f WHERE f.blob_hash = b.hash)",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|(h, s)| BlobHash::parse(&h).ok().map(|h| (h, s as u64))).collect())
    }

    /// Remove a blob record if it is still a GC candidate. Returns whether it was removed.
    pub async fn delete_blob_if_unreferenced(&self, hash: &BlobHash, cutoff: &str) -> Result<bool> {
        let r = sqlx::query(
            "DELETE FROM blobs WHERE hash = $1 AND last_seen_at < $2 \
             AND NOT EXISTS (SELECT 1 FROM version_files f WHERE f.blob_hash = $1)",
        )
        .bind(hash.hex())
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    // ---------------- versions ----------------

    pub async fn create_version(&self, dataset: &Dataset, v: NewVersion) -> Result<Version> {
        let mut seen = HashSet::new();
        for f in &v.files {
            if !dataset_core::validate_path(&f.path) {
                return Err(MetaError::Invalid(format!("invalid path {:?}", f.path)));
            }
            if !seen.insert(f.path.as_str()) {
                return Err(MetaError::Invalid(format!("duplicate path {:?}", f.path)));
            }
        }
        let hashes: Vec<BlobHash> = v.files.iter().map(|f| f.blob.clone()).collect();
        let missing = self.missing_blobs(&hashes).await?;
        if !missing.is_empty() {
            return Err(MetaError::MissingBlobs(missing));
        }
        for f in &v.files {
            if let Some(size) = self.blob_size(&f.blob).await? {
                if size != f.size {
                    return Err(MetaError::Invalid(format!("{}: size {} does not match blob size {size}", f.path, f.size)));
                }
            }
        }
        if let Some(parent) = &v.parent {
            self.get_version(dataset, parent).await?;
        }
        if let Some(b) = &v.branch {
            if let Some(r) = self.get_ref(dataset, b).await? {
                if r.kind != RefKind::Branch.as_str() {
                    return Err(MetaError::Conflict(format!("{b} is a tag")));
                }
            }
        }

        let id = dataset_core::new_version_id();
        let now = dataset_core::now();
        let manifest_hash = dataset_core::manifest_hash(&v.files);
        let total: u64 = v.files.iter().map(|f| f.size).sum();
        let producer_json = v.producer.as_ref().map(|p| serde_json::to_string(p).expect("serializable"));

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO dataset_versions (id, dataset_id, parent_id, manifest_hash, schema_hash, metadata, \
             file_count, total_size, producer, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&id)
        .bind(&dataset.id)
        .bind(v.parent.clone())
        .bind(manifest_hash.to_string())
        .bind(v.schema_hash.clone())
        .bind(v.metadata.to_string())
        .bind(v.files.len() as i64)
        .bind(total as i64)
        .bind(producer_json.clone())
        .bind(&v.created_by)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        for chunk in v.files.chunks(INSERT_CHUNK) {
            let values: Vec<String> = (0..chunk.len())
                .map(|i| format!("({})", placeholders(1 + i * 4, 4)))
                .collect();
            let sql = format!("INSERT INTO version_files (version_id, path, blob_hash, size) VALUES {}", values.join(", "));
            let mut q = sqlx::query(&sql);
            for f in chunk {
                q = q.bind(&id).bind(&f.path).bind(f.blob.hex().to_string()).bind(f.size as i64);
            }
            q.execute(&mut *tx).await?;
        }

        let out_uri = dataset_uri(&dataset.name, &id);
        let mut edges: Vec<LineageEdge> = v
            .inputs
            .iter()
            .map(|input| LineageEdge { from: input.clone(), to: out_uri.clone(), kind: EdgeKind::InputOf, producer: v.producer.clone() })
            .collect();
        if let Some(p) = &v.producer {
            edges.push(LineageEdge {
                from: out_uri.clone(),
                to: format!("{}://{}", p.kind, p.id),
                kind: EdgeKind::GeneratedBy,
                producer: None,
            });
        }
        for e in &edges {
            insert_edge(&mut *tx, e).await?;
        }

        if let Some(b) = &v.branch {
            upsert_ref(&mut *tx, &dataset.id, b, RefKind::Branch, &id).await?;
        }
        tx.commit().await?;
        self.get_version(dataset, &id).await
    }

    pub async fn get_version(&self, dataset: &Dataset, version_id: &str) -> Result<Version> {
        let row: VersionRow = sqlx::query_as(&format!(
            "SELECT {VERSION_COLS} FROM dataset_versions v JOIN datasets d ON d.id = v.dataset_id \
             WHERE v.dataset_id = $1 AND v.id = $2"
        ))
        .bind(&dataset.id)
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(MetaError::NotFound)?;
        row.try_into()
    }

    pub async fn list_versions(&self, dataset: &Dataset, limit: i64) -> Result<Vec<Version>> {
        let rows: Vec<VersionRow> = sqlx::query_as(&format!(
            "SELECT {VERSION_COLS} FROM dataset_versions v JOIN datasets d ON d.id = v.dataset_id \
             WHERE v.dataset_id = $1 ORDER BY v.id DESC LIMIT $2"
        ))
        .bind(&dataset.id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Resolve a version id, tag, branch or `latest`.
    pub async fn resolve(&self, dataset: &Dataset, reference: &str) -> Result<Version> {
        if reference == "latest" {
            return self.list_versions(dataset, 1).await?.into_iter().next().ok_or(MetaError::NotFound);
        }
        if let Some(r) = self.get_ref(dataset, reference).await? {
            return self.get_version(dataset, &r.version_id).await;
        }
        self.get_version(dataset, reference).await
    }

    pub async fn list_files(&self, version_id: &str, offset: i64, limit: i64) -> Result<Vec<ManifestFile>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT path, blob_hash, size FROM version_files WHERE version_id = $1 ORDER BY path LIMIT $2 OFFSET $3",
        )
        .bind(version_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(path, h, size)| {
                Ok(ManifestFile { path, blob: BlobHash::parse(&h).map_err(|e| MetaError::Invalid(e.to_string()))?, size: size as u64 })
            })
            .collect()
    }

    pub async fn all_files(&self, version_id: &str) -> Result<Vec<ManifestFile>> {
        self.list_files(version_id, 0, i64::MAX).await
    }

    pub async fn get_file(&self, version_id: &str, path: &str) -> Result<ManifestFile> {
        let (path, h, size): (String, String, i64) =
            sqlx::query_as("SELECT path, blob_hash, size FROM version_files WHERE version_id = $1 AND path = $2")
                .bind(version_id)
                .bind(path)
                .fetch_optional(&self.pool)
                .await?
                .ok_or(MetaError::NotFound)?;
        Ok(ManifestFile { path, blob: BlobHash::parse(&h).map_err(|e| MetaError::Invalid(e.to_string()))?, size: size as u64 })
    }

    /// Delete a version that no ref points to. Its blobs become GC candidates
    /// only after the safety window (their last_seen_at is bumped now).
    pub async fn delete_version(&self, dataset: &Dataset, version_id: &str) -> Result<()> {
        self.get_version(dataset, version_id).await?;
        let refs: Vec<String> = sqlx::query_scalar("SELECT name FROM refs WHERE version_id = $1")
            .bind(version_id)
            .fetch_all(&self.pool)
            .await?;
        if !refs.is_empty() {
            return Err(MetaError::Conflict(format!("version is referenced by {}", refs.join(", "))));
        }
        let now = dataset_core::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE blobs SET last_seen_at = $1 WHERE hash IN (SELECT blob_hash FROM version_files WHERE version_id = $2)")
            .bind(&now)
            .bind(version_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM version_files WHERE version_id = $1").bind(version_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM dataset_versions WHERE id = $1").bind(version_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------- refs ----------------

    pub async fn get_ref(&self, dataset: &Dataset, name: &str) -> Result<Option<RefEntry>> {
        Ok(sqlx::query_as("SELECT name, kind, version_id, updated_at FROM refs WHERE dataset_id = $1 AND name = $2")
            .bind(&dataset.id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn list_refs(&self, dataset: &Dataset) -> Result<Vec<RefEntry>> {
        Ok(sqlx::query_as("SELECT name, kind, version_id, updated_at FROM refs WHERE dataset_id = $1 ORDER BY kind, name")
            .bind(&dataset.id)
            .fetch_all(&self.pool)
            .await?)
    }

    /// Create a tag (fails if the name exists) or create/move a branch.
    pub async fn set_ref(&self, dataset: &Dataset, name: &str, kind: RefKind, version_id: &str) -> Result<RefEntry> {
        if !dataset_core::validate_name(name) || name == "latest" || ulid::Ulid::from_string(name).is_ok() {
            return Err(MetaError::Invalid(format!("invalid ref name {name:?}")));
        }
        self.get_version(dataset, version_id).await?;
        if let Some(existing) = self.get_ref(dataset, name).await? {
            if existing.kind == RefKind::Tag.as_str() || kind == RefKind::Tag {
                return Err(MetaError::Conflict(format!("{} {name} already exists", existing.kind)));
            }
        }
        let mut conn = self.pool.acquire().await?;
        upsert_ref(&mut *conn, &dataset.id, name, kind, version_id).await?;
        self.get_ref(dataset, name).await?.ok_or(MetaError::NotFound)
    }

    pub async fn delete_branch(&self, dataset: &Dataset, name: &str) -> Result<()> {
        let r = sqlx::query("DELETE FROM refs WHERE dataset_id = $1 AND name = $2 AND kind = 'branch'")
            .bind(&dataset.id)
            .bind(name)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(MetaError::NotFound);
        }
        Ok(())
    }

    // ---------------- lineage ----------------

    pub async fn add_edges(&self, edges: &[LineageEdge]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for e in edges {
            insert_edge(&mut *tx, e).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Neighborhood of `uri` up to `depth` hops, following edges in both directions.
    pub async fn lineage(&self, uri: &str, depth: usize) -> Result<LineageGraph> {
        let mut nodes = BTreeSet::from([uri.to_string()]);
        let mut edge_ids = HashSet::new();
        let mut edges = Vec::new();
        let mut queue = VecDeque::from([(uri.to_string(), 0usize)]);
        while let Some((node, d)) = queue.pop_front() {
            if d >= depth || nodes.len() > 10_000 {
                continue;
            }
            let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
                "SELECT id, from_uri, to_uri, kind, producer FROM lineage_edges WHERE from_uri = $1 OR to_uri = $1",
            )
            .bind(&node)
            .fetch_all(&self.pool)
            .await?;
            for (id, from, to, kind, producer) in rows {
                if !edge_ids.insert(id) {
                    continue;
                }
                for n in [&from, &to] {
                    if nodes.insert(n.clone()) {
                        queue.push_back((n.clone(), d + 1));
                    }
                }
                edges.push(LineageEdge {
                    from,
                    to,
                    kind: EdgeKind::parse(&kind).unwrap_or(EdgeKind::DerivedFrom),
                    producer: producer.and_then(|p| serde_json::from_str(&p).ok()),
                });
            }
        }
        Ok(LineageGraph { root: uri.to_string(), nodes: nodes.into_iter().collect(), edges })
    }
}

async fn insert_edge<'c, E: Executor<'c, Database = sqlx::Any>>(ex: E, e: &LineageEdge) -> Result<()> {
    sqlx::query("INSERT INTO lineage_edges (id, from_uri, to_uri, kind, producer, created_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(ulid::Ulid::new().to_string())
        .bind(&e.from)
        .bind(&e.to)
        .bind(e.kind.as_str())
        .bind(e.producer.as_ref().map(|p| serde_json::to_string(p).expect("serializable")))
        .bind(dataset_core::now())
        .execute(ex)
        .await?;
    Ok(())
}

async fn upsert_ref<'c, E: Executor<'c, Database = sqlx::Any>>(
    ex: E,
    dataset_id: &str,
    name: &str,
    kind: RefKind,
    version_id: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO refs (dataset_id, name, kind, version_id, updated_at) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (dataset_id, name) DO UPDATE SET version_id = excluded.version_id, updated_at = excluded.updated_at",
    )
    .bind(dataset_id)
    .bind(name)
    .bind(kind.as_str())
    .bind(version_id)
    .bind(dataset_core::now())
    .execute(ex)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile { path: path.into(), blob: BlobHash::of(data), size: data.len() as u64 }
    }

    fn new_version(files: Vec<ManifestFile>, parent: Option<String>, branch: Option<&str>) -> NewVersion {
        NewVersion {
            parent,
            files,
            schema_hash: None,
            metadata: serde_json::json!({"format": "parquet"}),
            producer: Some(Producer { kind: "pipeline".into(), id: "run-1".into() }),
            inputs: vec!["dataset://raw/01ABC".into()],
            branch: branch.map(String::from),
            created_by: "alice".into(),
        }
    }

    #[tokio::test]
    async fn versions_refs_lineage_gc() {
        let path = std::env::temp_dir().join(format!("ds-meta-{}.db", ulid::Ulid::new()));
        let meta = MetaStore::connect(&format!("sqlite://{}?mode=rwc", path.display())).await.unwrap();
        let ds = meta.create_dataset("multicredit", "", "alice").await.unwrap();
        let empty = meta.list_dataset_summaries(None).await.unwrap();
        assert_eq!((empty[0].version_count, empty[0].latest_id.clone()), (0, None));

        let (a, b, c) = (file("a.parquet", b"A"), file("b.parquet", b"B"), file("c.parquet", b"C"));
        // Blobs must be uploaded before a version can reference them.
        let err = meta.create_version(&ds, new_version(vec![a.clone()], None, None)).await.unwrap_err();
        assert!(matches!(err, MetaError::MissingBlobs(ref m) if m.len() == 1));
        for f in [&a, &b, &c] {
            meta.record_blob(&f.blob, f.size).await.unwrap();
        }

        let v1 = meta.create_version(&ds, new_version(vec![a.clone(), b.clone()], None, Some("training"))).await.unwrap();
        let v2 = meta
            .create_version(&ds, new_version(vec![a.clone(), c.clone()], Some(v1.id.clone()), Some("training")))
            .await
            .unwrap();
        assert_eq!(meta.resolve(&ds, "training").await.unwrap().id, v2.id);
        assert_eq!(meta.resolve(&ds, "latest").await.unwrap().id, v2.id);
        assert_eq!(meta.all_files(&v1.id).await.unwrap().len(), 2);
        let s = &meta.list_dataset_summaries(Some("MULTI")).await.unwrap()[0];
        assert_eq!((s.version_count, s.latest_id.as_deref(), s.latest_files), (2, Some(v2.id.as_str()), Some(2)));

        meta.set_ref(&ds, "v1-frozen", RefKind::Tag, &v1.id).await.unwrap();
        assert!(meta.set_ref(&ds, "v1-frozen", RefKind::Tag, &v2.id).await.is_err());
        assert!(meta.delete_version(&ds, &v1.id).await.is_err()); // tagged

        let g = meta.lineage(&dataset_uri("multicredit", &v2.id), 2).await.unwrap();
        assert!(g.nodes.contains(&"dataset://raw/01ABC".to_string()));
        assert!(g.nodes.contains(&"pipeline://run-1".to_string()));

        // Nothing is collectable: all blobs referenced or recently seen.
        assert!(meta.gc_candidates("9999-01-01T00:00:00Z").await.unwrap().is_empty());
        let orphan = file("x", b"orphan");
        meta.record_blob(&orphan.blob, orphan.size).await.unwrap();
        assert!(meta.gc_candidates("2000-01-01T00:00:00Z").await.unwrap().is_empty()); // inside window
        assert_eq!(meta.gc_candidates("9999-01-01T00:00:00Z").await.unwrap().len(), 1);

        drop(meta);
        let _ = std::fs::remove_file(path);
    }
}
