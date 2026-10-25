CREATE TABLE snapshot_removals (
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, name, version)
);
CREATE TABLE management_previews (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL REFERENCES admin_users(id),
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    expected_head TEXT NOT NULL REFERENCES snapshots(id),
    operation TEXT NOT NULL,
    selection TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'prepared',
    commit_key TEXT,
    committed_snapshot TEXT REFERENCES snapshots(id)
);
PRAGMA user_version = 8;
