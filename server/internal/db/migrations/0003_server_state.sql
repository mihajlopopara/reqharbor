-- Rate limits are in-process now (06 §5.9).
DROP TABLE rate_limits;

-- Keys: 'instance_id' and 'sync_epoch' (ULIDs, generated at first start; restore rotates sync_epoch),
-- 'restored_at', 'restored_from' (set by restore), 'throttle_key' (S4: 32 random bytes, base64).
CREATE TABLE server_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
