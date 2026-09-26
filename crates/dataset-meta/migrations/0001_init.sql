-- Portable schema: runs unchanged on PostgreSQL and SQLite.
-- Timestamps are fixed-width UTC TEXT (YYYY-MM-DDTHH:MM:SSZ), so they compare as strings.

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id),
    name       TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);

CREATE TABLE datasets (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    owner       TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

-- Every blob known to the store. last_seen_at is bumped whenever a blob is
-- uploaded, checked, or (un)referenced; GC only removes blobs that are
-- unreferenced AND unseen for the safety window.
CREATE TABLE blobs (
    hash         TEXT PRIMARY KEY,
    size         BIGINT NOT NULL,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

-- Immutable once inserted.
CREATE TABLE dataset_versions (
    id            TEXT PRIMARY KEY,
    dataset_id    TEXT NOT NULL REFERENCES datasets(id),
    parent_id     TEXT,
    manifest_hash TEXT NOT NULL,
    schema_hash   TEXT,
    metadata      TEXT NOT NULL,
    file_count    BIGINT NOT NULL,
    total_size    BIGINT NOT NULL,
    producer      TEXT,
    created_by    TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX dataset_versions_dataset ON dataset_versions (dataset_id, id);

CREATE TABLE version_files (
    version_id TEXT NOT NULL REFERENCES dataset_versions(id),
    path       TEXT NOT NULL,
    blob_hash  TEXT NOT NULL,
    size       BIGINT NOT NULL,
    PRIMARY KEY (version_id, path)
);
CREATE INDEX version_files_blob ON version_files (blob_hash);

-- Tags (immutable) and branches (mutable pointers).
CREATE TABLE refs (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    version_id TEXT NOT NULL REFERENCES dataset_versions(id),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (dataset_id, name)
);

CREATE TABLE lineage_edges (
    id         TEXT PRIMARY KEY,
    from_uri   TEXT NOT NULL,
    to_uri     TEXT NOT NULL,
    kind       TEXT NOT NULL,
    producer   TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX lineage_edges_from ON lineage_edges (from_uri);
CREATE INDEX lineage_edges_to ON lineage_edges (to_uri);
