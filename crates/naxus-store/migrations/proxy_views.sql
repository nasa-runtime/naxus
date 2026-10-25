ALTER TABLE proxy_sessions ADD COLUMN upstream_start INTEGER NOT NULL DEFAULT 0 CHECK (upstream_start BETWEEN 0 AND 15);
PRAGMA user_version = 10;
