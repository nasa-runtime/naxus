ALTER TABLE objects ADD COLUMN state TEXT NOT NULL DEFAULT 'present' CHECK (state IN ('present', 'deleting', 'purged'));
ALTER TABLE objects ADD COLUMN generation INTEGER NOT NULL DEFAULT 1;
CREATE TABLE artifact_access_revocations (
    revision_id TEXT PRIMARY KEY REFERENCES revisions(id),
    deletion_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE retention_pins (
    id TEXT PRIMARY KEY,
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    actor TEXT NOT NULL,
    reason TEXT NOT NULL
);
CREATE TABLE deletion_previews (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL REFERENCES admin_users(id),
    scope TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'prepared',
    task_id TEXT
);
CREATE TABLE object_deletions (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL,
    scope TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'running', 'completed', 'failed')),
    released_bytes INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE deletion_items (
    deletion_id TEXT NOT NULL REFERENCES object_deletions(id),
    digest TEXT NOT NULL REFERENCES objects(sha256),
    generation INTEGER NOT NULL,
    size INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('deleting', 'purged', 'shared')),
    PRIMARY KEY (deletion_id, digest)
);
CREATE TABLE proxy_session_objects (
    session_id TEXT NOT NULL REFERENCES proxy_sessions(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    digest TEXT NOT NULL,
    PRIMARY KEY (session_id, name, digest)
);
PRAGMA user_version = 9;
