CREATE TABLE proxy_sessions (
    id TEXT PRIMARY KEY,
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    actor TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('cache-first', 'revalidate')),
    expires_at INTEGER NOT NULL
);
CREATE TABLE proxy_session_entries (
    session_id TEXT NOT NULL REFERENCES proxy_sessions(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    body BLOB NOT NULL,
    PRIMARY KEY (session_id, name)
);
CREATE TABLE proxy_validators (
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    name TEXT NOT NULL,
    upstream TEXT NOT NULL,
    etag TEXT,
    modified TEXT,
    body BLOB NOT NULL,
    PRIMARY KEY (proxy_id, name, upstream)
);
CREATE TABLE proxy_missing (
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    name TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (proxy_id, name)
);
CREATE TABLE proxy_materializations (
    id TEXT PRIMARY KEY,
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    actor TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    valid INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX valid_materialization_graph ON proxy_materializations(proxy_id, actor, fingerprint) WHERE valid = 1;
CREATE TABLE materialization_items (
    materialization_id TEXT NOT NULL REFERENCES proxy_materializations(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    digest TEXT NOT NULL,
    PRIMARY KEY (materialization_id, name, version)
);
PRAGMA user_version = 6;
