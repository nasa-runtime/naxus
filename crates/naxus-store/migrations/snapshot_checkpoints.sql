CREATE TABLE snapshot_checkpoints (
    snapshot_id TEXT PRIMARY KEY REFERENCES snapshots(id)
);
CREATE TABLE checkpoint_entries (
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revisions(id),
    PRIMARY KEY (snapshot_id, name, version)
);
PRAGMA user_version = 2;
