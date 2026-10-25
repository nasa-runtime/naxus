ALTER TABLE objects ADD COLUMN installed_at INTEGER NOT NULL DEFAULT 0;
UPDATE objects SET installed_at = unixepoch();
CREATE INDEX object_installation_age ON objects(state, installed_at);
CREATE INDEX revision_object_references ON revisions(sha256);
CREATE INDEX proxy_object_references ON proxy_objects(digest);
CREATE INDEX session_object_references ON proxy_session_objects(digest);
CREATE INDEX materialization_object_references ON materialization_items(digest);
CREATE INDEX batch_object_references ON batch_items(json_extract(entry_json, '$.cksum'));
PRAGMA user_version = 13;
