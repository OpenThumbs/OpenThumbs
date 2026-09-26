use std::ops::Range;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use dataset_core::{dataset_uri, diff, BlobHash, DatasetRef, LineageEdge, Manifest, ManifestFile, Producer, RefKind};
use dataset_meta::{Dataset, NewVersion, Version};
use dataset_storage::ByteStream;
use futures_util::TryStreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{self, Reader, Writer};
use crate::error::{AppError, AppResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/auth/token", post(login))
        .route("/auth/whoami", get(whoami))
        .route("/auth/login", post(session_login))
        .route("/auth/logout", post(session_logout))
        .route("/auth/tokens", get(list_tokens).post(create_token))
        .route("/auth/tokens/{id}", delete(revoke_token))
        .route("/datasets", get(list_datasets).post(create_dataset))
        .route("/datasets/{name}", get(get_dataset))
        .route("/datasets/{name}/versions", get(list_versions).post(create_version))
        .route("/datasets/{name}/versions/{reference}", get(get_version).delete(delete_version))
        .route("/datasets/{name}/versions/{reference}/manifest", get(get_manifest))
        .route("/datasets/{name}/versions/{reference}/files", get(list_files))
        .route("/datasets/{name}/versions/{reference}/files/{*path}", get(read_file).head(read_file))
        .route("/datasets/{name}/versions/{reference}/lineage", get(version_lineage))
        .route("/datasets/{name}/resolve/{reference}", get(resolve))
        .route("/datasets/{name}/refs", get(list_refs))
        .route("/datasets/{name}/tags", post(create_tag))
        .route("/datasets/{name}/branches", post(set_branch))
        .route("/datasets/{name}/branches/{*branch}", delete(delete_branch))
        .route("/compare/{name}/{v1}/{v2}", get(compare))
        .route("/lineage", get(lineage).post(add_lineage))
        .route("/blobs/missing", post(missing_blobs))
        .route("/blobs/{hash}", get(get_blob).head(get_blob).put(put_blob))
        .route("/uploads", post(create_upload))
        .route("/uploads/{id}", get(upload_status).delete(abort_upload))
        .route("/uploads/{id}/parts/{part}", put(put_part))
        .route("/uploads/{id}/complete", post(complete_upload))
        .route("/admin/gc", post(gc))
}

fn body_stream(body: Body) -> ByteStream {
    Box::pin(body.into_data_stream().map_err(std::io::Error::other))
}

fn parse_hash(s: &str) -> AppResult<BlobHash> {
    BlobHash::parse(s).map_err(|e| AppError::BadRequest(e.to_string()))
}

async fn load(state: &AppState, name: &str) -> AppResult<Dataset> {
    Ok(state.meta.get_dataset(name).await?)
}

async fn load_version(state: &AppState, name: &str, reference: &str) -> AppResult<(Dataset, Version)> {
    let ds = load(state, name).await?;
    let v = state.meta.resolve(&ds, reference).await?;
    Ok((ds, v))
}

/// Normalize a lineage input: `dataset@ref` is pinned to a concrete version URI
/// so lineage always points at exact bytes; other URIs are kept as-is.
async fn pin_input(state: &AppState, input: &str) -> AppResult<String> {
    if input.contains("://") {
        return Ok(input.to_string());
    }
    let r = DatasetRef::parse(input).ok_or_else(|| AppError::BadRequest(format!("invalid input {input:?}")))?;
    let (ds, v) = load_version(state, &r.dataset, &r.reference).await?;
    Ok(dataset_uri(&ds.name, &v.id))
}

// ---------------- misc & auth ----------------

async fn index() -> Json<Value> {
    Json(json!({ "service": "dataset-store", "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    #[serde(default)]
    name: Option<String>,
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> AppResult<Json<Value>> {
    let user = state.meta.user_by_name(&req.username).await?.ok_or(AppError::Unauthorized)?;
    if !auth::verify_password(&req.password, &user.password_hash) {
        return Err(AppError::Unauthorized);
    }
    let token = auth::issue_token(&state.meta, &user, req.name.as_deref().unwrap_or("cli")).await?;
    Ok(Json(json!({ "token": token, "username": user.username })))
}

async fn whoami(State(state): State<AppState>, Writer(user): Writer) -> Json<Value> {
    Json(json!({ "username": user.username, "admin": state.is_admin(&user), "public_read": state.config.public_read }))
}

fn session_cookie(value: &str, max_age: u64) -> String {
    format!("{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}", auth::SESSION_COOKIE)
}

/// Browser sign-in: issues a session token in an HttpOnly cookie.
async fn session_login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> AppResult<Response> {
    let user = state.meta.user_by_name(&req.username).await?.ok_or(AppError::Unauthorized)?;
    if !auth::verify_password(&req.password, &user.password_hash) {
        return Err(AppError::Unauthorized);
    }
    let token = auth::issue_token(&state.meta, &user, "web-session").await?;
    let cookie = session_cookie(&token, 60 * 60 * 24 * 30);
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "username": user.username, "admin": state.is_admin(&user) }))).into_response())
}

async fn session_logout(State(state): State<AppState>, parts: axum::http::request::Parts) -> AppResult<Response> {
    if let Some(token) = auth::session_token(&parts) {
        state.meta.delete_token_by_hash(&auth::hash_token(&token)).await?;
    }
    Ok(([(header::SET_COOKIE, session_cookie("", 0))], StatusCode::NO_CONTENT).into_response())
}

async fn list_tokens(State(state): State<AppState>, Writer(user): Writer) -> AppResult<Json<Value>> {
    let tokens: Vec<_> = state.meta.list_tokens(&user.id).await?.into_iter().filter(|t| t.name != "web-session").collect();
    Ok(Json(json!(tokens)))
}

#[derive(Deserialize)]
struct NewToken {
    name: String,
}

async fn create_token(State(state): State<AppState>, Writer(user): Writer, Json(req): Json<NewToken>) -> AppResult<Response> {
    let name = req.name.trim();
    if name.is_empty() || name == "web-session" || name.len() > 64 {
        return Err(AppError::BadRequest("invalid token name".into()));
    }
    let token = auth::issue_token(&state.meta, &user, name).await?;
    Ok((StatusCode::CREATED, Json(json!({ "name": name, "token": token }))).into_response())
}

async fn revoke_token(State(state): State<AppState>, Writer(user): Writer, Path(id): Path<String>) -> AppResult<StatusCode> {
    state.meta.delete_token(&user.id, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- datasets ----------------

#[derive(Deserialize)]
struct SearchQuery {
    search: Option<String>,
}

/// Datasets with version count and a latest-version summary (for catalog views).
async fn list_datasets(State(state): State<AppState>, _: Reader, Query(q): Query<SearchQuery>) -> AppResult<Json<Vec<Value>>> {
    let rows = state.meta.list_dataset_summaries(q.search.as_deref()).await?;
    Ok(Json(
        rows.into_iter()
            .map(|d| {
                let latest = d.latest_id.map(|id| {
                    json!({
                        "id": id,
                        "created_at": d.latest_created_at,
                        "total_size": d.latest_size,
                        "file_count": d.latest_files,
                        "created_by": d.latest_by,
                        "metadata": d.latest_metadata.and_then(|m| serde_json::from_str::<Value>(&m).ok()),
                    })
                });
                json!({
                    "id": d.id,
                    "name": d.name,
                    "description": d.description,
                    "owner": d.owner,
                    "created_at": d.created_at,
                    "version_count": d.version_count,
                    "latest": latest,
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct CreateDataset {
    name: String,
    #[serde(default)]
    description: String,
}

async fn create_dataset(State(state): State<AppState>, Writer(user): Writer, Json(req): Json<CreateDataset>) -> AppResult<Response> {
    if !dataset_core::validate_name(&req.name) || req.name.contains('/') {
        return Err(AppError::BadRequest("dataset names may contain letters, digits, '-', '_' and '.'".into()));
    }
    let ds = state.meta.create_dataset(&req.name, &req.description, &user.username).await?;
    Ok((StatusCode::CREATED, Json(ds)).into_response())
}

async fn get_dataset(State(state): State<AppState>, _: Reader, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let ds = load(&state, &name).await?;
    let refs = state.meta.list_refs(&ds).await?;
    let latest = state.meta.list_versions(&ds, 1).await?.into_iter().next();
    Ok(Json(json!({ "dataset": ds, "refs": refs, "latest": latest })))
}

// ---------------- versions ----------------

#[derive(Deserialize)]
struct CreateVersion {
    files: Vec<ManifestFile>,
    /// Parent version (id, tag, branch or `latest`).
    parent: Option<String>,
    schema_hash: Option<String>,
    #[serde(default)]
    metadata: Value,
    producer: Option<Producer>,
    /// `dataset@ref` (pinned to a version) or any URI.
    #[serde(default)]
    inputs: Vec<String>,
    /// Branch to move to the new version.
    branch: Option<String>,
}

async fn create_version(
    State(state): State<AppState>,
    Writer(user): Writer,
    Path(name): Path<String>,
    Json(req): Json<CreateVersion>,
) -> AppResult<Response> {
    let _gc = state.gc_lock.read().await;
    let ds = load(&state, &name).await?;
    let parent = match &req.parent {
        Some(p) => Some(state.meta.resolve(&ds, p).await?.id),
        None => None,
    };
    if let Some(b) = &req.branch {
        if !dataset_core::validate_name(b) || b == "latest" {
            return Err(AppError::BadRequest(format!("invalid branch name {b:?}")));
        }
    }
    let mut inputs = Vec::with_capacity(req.inputs.len());
    for i in &req.inputs {
        inputs.push(pin_input(&state, i).await?);
    }
    let metadata = if req.metadata.is_null() { json!({}) } else { req.metadata };
    let v = state
        .meta
        .create_version(
            &ds,
            NewVersion {
                parent,
                files: req.files,
                schema_hash: req.schema_hash,
                metadata,
                producer: req.producer,
                inputs,
                branch: req.branch,
                created_by: user.username,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<i64>,
}

async fn list_versions(
    State(state): State<AppState>,
    _: Reader,
    Path(name): Path<String>,
    Query(q): Query<LimitQuery>,
) -> AppResult<Json<Vec<Version>>> {
    let ds = load(&state, &name).await?;
    Ok(Json(state.meta.list_versions(&ds, q.limit.unwrap_or(100).clamp(1, 10_000)).await?))
}

async fn get_version(State(state): State<AppState>, _: Reader, Path((name, reference)): Path<(String, String)>) -> AppResult<Json<Version>> {
    Ok(Json(load_version(&state, &name, &reference).await?.1))
}

async fn delete_version(
    State(state): State<AppState>,
    Writer(user): Writer,
    Path((name, id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let ds = load(&state, &name).await?;
    if ds.owner != user.username && !state.is_admin(&user) {
        return Err(AppError::Forbidden);
    }
    state.meta.delete_version(&ds, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_manifest(State(state): State<AppState>, _: Reader, Path((name, reference)): Path<(String, String)>) -> AppResult<Json<Manifest>> {
    let (ds, v) = load_version(&state, &name, &reference).await?;
    let files = state.meta.all_files(&v.id).await?;
    Ok(Json(Manifest {
        dataset: ds.name,
        version: v.id,
        parent: v.parent,
        manifest_hash: v.manifest_hash,
        files,
        schema_hash: v.schema_hash,
        metadata: v.metadata,
    }))
}

#[derive(Deserialize)]
struct PageQuery {
    offset: Option<i64>,
    limit: Option<i64>,
}

async fn list_files(
    State(state): State<AppState>,
    _: Reader,
    Path((name, reference)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> AppResult<Json<Value>> {
    let (_, v) = load_version(&state, &name, &reference).await?;
    let offset = q.offset.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(1000).clamp(1, 100_000);
    let files = state.meta.list_files(&v.id, offset, limit).await?;
    Ok(Json(json!({ "version": v.id, "total": v.file_count, "offset": offset, "files": files })))
}

async fn read_file(
    State(state): State<AppState>,
    _: Reader,
    method: Method,
    headers: HeaderMap,
    Path((name, reference, path)): Path<(String, String, String)>,
) -> AppResult<Response> {
    let (_, v) = load_version(&state, &name, &reference).await?;
    let f = state.meta.get_file(&v.id, &path).await?;
    serve_blob(&state, &f.blob, method == Method::HEAD, &headers).await
}

async fn resolve(State(state): State<AppState>, _: Reader, Path((name, reference)): Path<(String, String)>) -> AppResult<Json<Value>> {
    let (ds, v) = load_version(&state, &name, &reference).await?;
    let files = state.meta.all_files(&v.id).await?;
    let base = &state.config.base_url;
    let files: Vec<Value> = files
        .iter()
        .map(|f| json!({ "path": f.path, "blob": f.blob, "size": f.size, "url": format!("{base}/api/blobs/{}", f.blob) }))
        .collect();
    Ok(Json(json!({
        "dataset": ds.name,
        "dataset_id": ds.id,
        "reference": reference,
        "version": v.id,
        "manifest_hash": v.manifest_hash,
        "schema_hash": v.schema_hash,
        "uri": dataset_uri(&ds.name, &v.id),
        "files": files,
    })))
}

// ---------------- refs ----------------

async fn list_refs(State(state): State<AppState>, _: Reader, Path(name): Path<String>) -> AppResult<Json<Value>> {
    let ds = load(&state, &name).await?;
    Ok(Json(json!(state.meta.list_refs(&ds).await?)))
}

#[derive(Deserialize)]
struct SetRef {
    name: String,
    /// Version id, tag, branch or `latest`.
    version: String,
}

async fn set_ref(state: AppState, name: String, kind: RefKind, req: SetRef) -> AppResult<Response> {
    let ds = load(&state, &name).await?;
    let v = state.meta.resolve(&ds, &req.version).await?;
    let r = state.meta.set_ref(&ds, &req.name, kind, &v.id).await?;
    Ok((StatusCode::CREATED, Json(r)).into_response())
}

async fn create_tag(State(state): State<AppState>, _: Writer, Path(name): Path<String>, Json(req): Json<SetRef>) -> AppResult<Response> {
    set_ref(state, name, RefKind::Tag, req).await
}

async fn set_branch(State(state): State<AppState>, _: Writer, Path(name): Path<String>, Json(req): Json<SetRef>) -> AppResult<Response> {
    set_ref(state, name, RefKind::Branch, req).await
}

async fn delete_branch(State(state): State<AppState>, _: Writer, Path((name, branch)): Path<(String, String)>) -> AppResult<StatusCode> {
    let ds = load(&state, &name).await?;
    state.meta.delete_branch(&ds, &branch).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- compare & lineage ----------------

fn metadata_changes(old: &Value, new: &Value) -> Value {
    let (Some(o), Some(n)) = (old.as_object(), new.as_object()) else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for key in o.keys().chain(n.keys()) {
        let (a, b) = (o.get(key), n.get(key));
        if a != b && !out.contains_key(key) {
            out.insert(key.clone(), json!({ "old": a, "new": b }));
        }
    }
    Value::Object(out)
}

async fn compare(State(state): State<AppState>, _: Reader, Path((name, v1, v2)): Path<(String, String, String)>) -> AppResult<Json<Value>> {
    let ds = load(&state, &name).await?;
    let old = state.meta.resolve(&ds, &v1).await?;
    let new = state.meta.resolve(&ds, &v2).await?;
    let d = diff(&state.meta.all_files(&old.id).await?, &state.meta.all_files(&new.id).await?);
    Ok(Json(json!({
        "old": { "version": old.id, "manifest_hash": old.manifest_hash },
        "new": { "version": new.id, "manifest_hash": new.manifest_hash },
        "identical": old.manifest_hash == new.manifest_hash,
        "storage": d,
        "structure": {
            "schema_changed": old.schema_hash != new.schema_hash,
            "old_schema_hash": old.schema_hash,
            "new_schema_hash": new.schema_hash,
            "metadata_changes": metadata_changes(&old.metadata, &new.metadata),
        },
    })))
}

#[derive(Deserialize)]
struct DepthQuery {
    depth: Option<usize>,
}

async fn version_lineage(
    State(state): State<AppState>,
    _: Reader,
    Path((name, reference)): Path<(String, String)>,
    Query(q): Query<DepthQuery>,
) -> AppResult<Json<Value>> {
    let (ds, v) = load_version(&state, &name, &reference).await?;
    let g = state.meta.lineage(&dataset_uri(&ds.name, &v.id), q.depth.unwrap_or(3).min(20)).await?;
    Ok(Json(json!(g)))
}

#[derive(Deserialize)]
struct LineageQuery {
    uri: String,
    depth: Option<usize>,
}

async fn lineage(State(state): State<AppState>, _: Reader, Query(q): Query<LineageQuery>) -> AppResult<Json<Value>> {
    let g = state.meta.lineage(&q.uri, q.depth.unwrap_or(3).min(20)).await?;
    Ok(Json(json!(g)))
}

#[derive(Deserialize)]
struct AddLineage {
    edges: Vec<LineageEdge>,
}

/// Record external edges, e.g. `dataset@training USED_BY mlflow://run/abc`.
async fn add_lineage(State(state): State<AppState>, _: Writer, Json(req): Json<AddLineage>) -> AppResult<StatusCode> {
    let mut edges = Vec::with_capacity(req.edges.len());
    for mut e in req.edges {
        e.from = pin_input(&state, &e.from).await?;
        e.to = pin_input(&state, &e.to).await?;
        edges.push(e);
    }
    state.meta.add_edges(&edges).await?;
    Ok(StatusCode::CREATED)
}

// ---------------- blobs ----------------

#[derive(Deserialize)]
struct MissingRequest {
    blobs: Vec<BlobHash>,
}

async fn missing_blobs(State(state): State<AppState>, _: Writer, Json(req): Json<MissingRequest>) -> AppResult<Json<Value>> {
    let _gc = state.gc_lock.read().await;
    Ok(Json(json!({ "missing": state.meta.missing_blobs(&req.blobs).await? })))
}

/// Parse a single `Range: bytes=a-b | a- | -n` header against `len`.
fn parse_range(value: &str, len: u64) -> Option<Range<u64>> {
    let spec = value.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let r = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            len.saturating_sub(n)..len
        }
        (a, "") => a.parse().ok()?..len,
        (a, b) => a.parse().ok()?..(b.parse::<u64>().ok()? + 1).min(len),
    };
    (r.start < r.end && r.end <= len).then_some(r)
}

async fn serve_blob(state: &AppState, hash: &BlobHash, head: bool, req_headers: &HeaderMap) -> AppResult<Response> {
    let len = state.store.size(hash).await?.ok_or(AppError::NotFound)?;
    let range = match req_headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(r) => Some(parse_range(r, len).ok_or(AppError::RangeNotSatisfiable)?),
        None => None,
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::ETAG, HeaderValue::from_str(&format!("\"{hash}\"")).map_err(anyhow::Error::from)?);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    let (status, body_len) = match &range {
        Some(r) => {
            let cr = format!("bytes {}-{}/{len}", r.start, r.end - 1);
            headers.insert(header::CONTENT_RANGE, HeaderValue::from_str(&cr).map_err(anyhow::Error::from)?);
            (StatusCode::PARTIAL_CONTENT, r.end - r.start)
        }
        None => (StatusCode::OK, len),
    };
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body_len));
    if head {
        return Ok((status, headers).into_response());
    }
    let stream = state.store.get(hash, range).await?;
    Ok((status, headers, Body::from_stream(stream)).into_response())
}

async fn get_blob(State(state): State<AppState>, _: Reader, method: Method, headers: HeaderMap, Path(hash): Path<String>) -> AppResult<Response> {
    serve_blob(&state, &parse_hash(&hash)?, method == Method::HEAD, &headers).await
}

/// Single-request upload for small/medium blobs; the body must hash to `{hash}`.
async fn put_blob(State(state): State<AppState>, _: Writer, Path(hash): Path<String>, body: Body) -> AppResult<Json<Value>> {
    let hash = parse_hash(&hash)?;
    let _gc = state.gc_lock.read().await;
    let blob = state.store.put(body_stream(body), Some(&hash)).await?;
    state.meta.record_blob(&blob.hash, blob.size).await?;
    Ok(Json(json!(blob)))
}

// ---------------- multipart uploads ----------------

#[derive(Deserialize)]
struct CreateUpload {
    sha256: Option<BlobHash>,
    size: Option<u64>,
}

async fn create_upload(State(state): State<AppState>, _: Writer, Json(req): Json<CreateUpload>) -> AppResult<Response> {
    if let Some(h) = &req.sha256 {
        if state.meta.blob_size(h).await?.is_some() && state.store.exists(h).await? {
            // Already stored: nothing to upload.
            return Ok((StatusCode::OK, Json(json!({ "exists": true, "sha256": h }))).into_response());
        }
    }
    let up = state.uploads.create(req.sha256, req.size).await?;
    Ok((StatusCode::CREATED, Json(json!(up))).into_response())
}

async fn upload_status(State(state): State<AppState>, _: Writer, Path(id): Path<String>) -> AppResult<Json<Value>> {
    Ok(Json(json!(state.uploads.status(&id).await?)))
}

async fn put_part(State(state): State<AppState>, _: Writer, Path((id, part)): Path<(String, u32)>, body: Body) -> AppResult<Json<Value>> {
    Ok(Json(json!(state.uploads.put_part(&id, part, body_stream(body)).await?)))
}

async fn complete_upload(State(state): State<AppState>, _: Writer, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let _gc = state.gc_lock.read().await;
    let blob = state.uploads.complete(&id, state.store.as_ref()).await?;
    state.meta.record_blob(&blob.hash, blob.size).await?;
    Ok(Json(json!(blob)))
}

async fn abort_upload(State(state): State<AppState>, _: Writer, Path(id): Path<String>) -> AppResult<StatusCode> {
    state.uploads.abort(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- garbage collection ----------------

#[derive(Deserialize)]
struct GcRequest {
    #[serde(default = "default_true")]
    dry_run: bool,
    /// Safety window: only blobs unreferenced and unseen for this long are removed.
    #[serde(default = "default_gc_hours")]
    min_age_hours: u64,
}

fn default_true() -> bool {
    true
}

fn default_gc_hours() -> u64 {
    24
}

async fn gc(State(state): State<AppState>, Writer(user): Writer, Json(req): Json<GcRequest>) -> AppResult<Json<Value>> {
    if !state.is_admin(&user) {
        return Err(AppError::Forbidden);
    }
    // Exclusive: no uploads or version creation can race with deletion.
    let _gc = state.gc_lock.write().await;
    let cutoff = dataset_core::timestamp(time::OffsetDateTime::now_utc() - time::Duration::hours(req.min_age_hours as i64));
    let candidates = state.meta.gc_candidates(&cutoff).await?;
    let mut deleted = 0u64;
    let mut freed = 0u64;
    if !req.dry_run {
        for (hash, size) in &candidates {
            if state.meta.delete_blob_if_unreferenced(hash, &cutoff).await? {
                state.store.delete(hash).await?;
                deleted += 1;
                freed += size;
            }
        }
    }
    Ok(Json(json!({
        "dry_run": req.dry_run,
        "cutoff": cutoff,
        "candidates": candidates.len(),
        "candidate_bytes": candidates.iter().map(|(_, s)| s).sum::<u64>(),
        "deleted": deleted,
        "freed_bytes": freed,
    })))
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some(0..10));
        assert_eq!(parse_range("bytes=90-", 100), Some(90..100));
        assert_eq!(parse_range("bytes=-10", 100), Some(90..100));
        assert_eq!(parse_range("bytes=95-200", 100), Some(95..100));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
    }
}
