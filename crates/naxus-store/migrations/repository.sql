CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE repositories (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    policy TEXT NOT NULL CHECK (policy IN ('mutable', 'immutable')),
    head TEXT NOT NULL
);
CREATE TABLE snapshots (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    parent_id TEXT REFERENCES snapshots(id),
    manifest_sha256 TEXT NOT NULL
);
CREATE TABLE objects (
    sha256 TEXT PRIMARY KEY,
    size INTEGER NOT NULL CHECK (size >= 0)
);
CREATE TABLE crate_names (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    canonical TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (repository_id, canonical)
);
CREATE TABLE revisions (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    sha256 TEXT NOT NULL REFERENCES objects(sha256),
    metadata_sha256 TEXT NOT NULL,
    entry_json TEXT NOT NULL,
    actor TEXT NOT NULL
);
CREATE TABLE snapshot_changes (
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revisions(id),
    PRIMARY KEY (snapshot_id, name, version)
);
CREATE TABLE commits (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    actor TEXT NOT NULL,
    PRIMARY KEY (repository_id, idempotency_key)
);
CREATE TABLE audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX revision_coordinates ON revisions(repository_id, name, version);
PRAGMA user_version = 1;
