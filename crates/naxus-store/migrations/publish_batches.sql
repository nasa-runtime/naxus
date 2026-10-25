CREATE TABLE publish_batches (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    actor TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    expected_head TEXT NOT NULL REFERENCES snapshots(id),
    state TEXT NOT NULL CHECK (state IN ('open', 'validating', 'committed', 'aborted', 'expired')),
    snapshot_id TEXT REFERENCES snapshots(id),
    expires_at INTEGER NOT NULL,
    UNIQUE (repository_id, actor, idempotency_key)
);
CREATE TABLE batch_items (
    batch_id TEXT NOT NULL REFERENCES publish_batches(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    entry_json TEXT NOT NULL,
    PRIMARY KEY (batch_id, name, version)
);
CREATE INDEX open_batches ON publish_batches(actor, state, expires_at);
PRAGMA user_version = 3;
