CREATE TABLE retention_leases (
    id TEXT PRIMARY KEY,
    snapshot_set_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    actor TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX lease_expiry ON retention_leases(expires_at);
PRAGMA user_version = 4;
