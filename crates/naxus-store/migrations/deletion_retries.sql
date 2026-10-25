ALTER TABLE object_deletions ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE object_deletions ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE object_deletions ADD COLUMN last_error_code TEXT;
CREATE INDEX deletion_retry_due ON object_deletions(state, next_attempt_at);
PRAGMA user_version = 11;
