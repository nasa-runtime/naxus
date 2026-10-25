CREATE TABLE proxy_repositories (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    logical_index TEXT NOT NULL
);
CREATE TABLE proxy_indexes (
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    name TEXT NOT NULL,
    body BLOB NOT NULL,
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (proxy_id, name)
);
CREATE TABLE proxy_coordinates (
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    digest TEXT NOT NULL,
    PRIMARY KEY (proxy_id, name, version)
);
CREATE TABLE proxy_objects (
    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),
    digest TEXT NOT NULL REFERENCES objects(sha256),
    PRIMARY KEY (proxy_id, digest)
);
PRAGMA user_version = 5;
