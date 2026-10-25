CREATE TABLE admin_users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'operator', 'developer', 'reader')),
    repositories TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    must_change INTEGER NOT NULL DEFAULT 0,
    auth_version INTEGER NOT NULL DEFAULT 1,
    failures INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE admin_sessions (
    digest TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES admin_users(id),
    csrf_digest TEXT NOT NULL,
    auth_version INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE TABLE access_tokens (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES admin_users(id),
    label TEXT NOT NULL,
    permission TEXT NOT NULL CHECK (permission IN ('read', 'publish')),
    repositories TEXT NOT NULL,
    auth_version INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE security_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
PRAGMA user_version = 7;
