ALTER TABLE access_tokens ADD COLUMN allow_overwrite INTEGER NOT NULL DEFAULT 0;
ALTER TABLE access_tokens ADD COLUMN publish_crates TEXT NOT NULL DEFAULT '[]';
PRAGMA user_version = 12;
