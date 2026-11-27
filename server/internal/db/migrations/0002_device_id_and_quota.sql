-- QA-R1-026 (contract C3): deterministic LWW tie-break. Each entity remembers the deviceId of the
-- change that wrote it; on equal updatedAt the lexicographically greater deviceId wins.
-- Rows written before this migration (and changes from old clients) carry deviceId "".
ALTER TABLE entities ADD COLUMN device_id TEXT NOT NULL DEFAULT '';

-- QA-R1-024: per-workspace storage quota. `size` is the UTF-8 byte length of the stored JSON
-- (entity data / share snapshot); tombstones and revoked shares are 0.
ALTER TABLE entities ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shares ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
UPDATE entities SET size = COALESCE(length(CAST(data AS BLOB)), 0);
UPDATE shares SET size = CASE WHEN revoked_at IS NULL THEN COALESCE(length(CAST(snapshot AS BLOB)), 0) ELSE 0 END;

-- Covering indexes so SUM(size) per workspace never touches the large data/snapshot columns.
CREATE INDEX entities_workspace_size ON entities (workspace_id, size);
CREATE INDEX shares_workspace_size ON shares (workspace_id, revoked_at, size);
