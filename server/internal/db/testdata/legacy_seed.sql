-- A populated legacy (Worker-era) database for the populated-DB migration test
-- (06-go-server.md §8.6, internal/app/migration_populated_test.go). It is loaded at schema v2,
-- after 0001 and 0002, before 0003 and 0004 run.
--
-- 3 workspaces and 1,000 entities: every type, revs with gaps, tombstones, rows without a
-- deviceId (written before 0002), non-ASCII data; 9 shares, 4 of them revoked; a rate_limits row
-- (0003 drops the table). The tokens are rh_Seed{One,Two,Six} followed by 36 "A".

INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES
  ('01JSEEDWS00000000000000001', 'Acme API', '2766366ecba3eee61295e4cf4438eaa459c9cc64261b79831f2815ba70126f36', 1200, 1790000000000),
  ('01JSEEDWS00000000000000002', 'Ünïcödé ワークスペース', '0c93432ea310fb25d44f787c38c8648b096b8cc178772393e7e81c8344083328', 610, 1790000100000),
  ('01JSEEDWS00000000000000003', 'Small', '7d09cff9ab65a1cf1a3d9f9a9c468351abaa0fdb8c6323a154ab11444adfcbbb', 100, 1790000200000);

-- Entities 1..600 in workspace 1 (rev 2i), 601..900 in workspace 2 (rev 2k + 10), 901..1000 in
-- workspace 3 (rev k). Every 7th is a tombstone; every 3rd has the pre-0002 deviceId "".
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000),
rows AS (
  SELECT i,
    CASE WHEN i <= 600 THEN '01JSEEDWS00000000000000001'
         WHEN i <= 900 THEN '01JSEEDWS00000000000000002'
         ELSE '01JSEEDWS00000000000000003' END AS ws,
    CASE WHEN i <= 600 THEN 2 * i
         WHEN i <= 900 THEN 2 * (i - 600) + 10
         ELSE i - 900 END AS rev,
    CASE i % 5 WHEN 0 THEN 'project' WHEN 1 THEN 'collection' WHEN 2 THEN 'folder' WHEN 3 THEN 'request' ELSE 'environment' END AS typ,
    printf('01JSEEDENT%016d', i) AS eid,
    i % 7 = 0 AS del
  FROM n
)
INSERT INTO entities (workspace_id, type, id, rev, updated_at, deleted, data, device_id, size)
SELECT ws, typ, eid, rev, 1790000000000 + i * 1000, del,
  CASE WHEN del THEN NULL
       ELSE printf('{"id":"%s","name":"%s %d café ☕","sortOrder":%d,"tags":["a","é"],"nested":{"n":%d.5,"ok":true,"none":null}}', eid, typ, i, i, i) END,
  CASE WHEN i % 3 = 0 THEN '' ELSE printf('01JSEEDDEV%016d', i % 4) END,
  0
FROM rows;
-- size has 0002's meaning: the UTF-8 byte length of the stored JSON, 0 for tombstones.
UPDATE entities SET size = COALESCE(length(CAST(data AS BLOB)), 0);

INSERT INTO shares (id, workspace_id, kind, entity_id, snapshot, created_at, updated_at, revoked_at) VALUES
  ('SeedShareW1Active00001', '01JSEEDWS00000000000000001', 'collection', '01JSEEDENT0000000000000001',
   '{"schemaVersion":1,"collection":{"id":"01JSEEDENT0000000000000001","name":"Users API","description":"Endpoints for users","auth":{"type":"bearer","token":"{{token}}"},"folders":[{"id":"F1","collectionId":"01JSEEDENT0000000000000001","parentFolderId":null,"name":"Admin","sortOrder":0}],"requests":[{"id":"R1","collectionId":"01JSEEDENT0000000000000001","folderId":null,"name":"List users","method":"GET","url":"{{baseUrl}}/users?page=1","params":[{"key":"page","value":"1","enabled":true}],"headers":[{"key":"Accept","value":"application/json","enabled":true}],"auth":{"type":"inherit"},"body":{"type":"none"},"settings":{"followRedirects":true,"timeoutMs":30000,"verifyTls":true},"sortOrder":0},{"id":"R2","collectionId":"01JSEEDENT0000000000000001","folderId":"F1","name":"Delete user","method":"DELETE","url":"{{baseUrl}}/users/1","params":[],"headers":[],"auth":{"type":"inherit"},"body":{"type":"none"},"settings":{},"sortOrder":0}]},"environment":{"name":"Prod","variables":[{"key":"baseUrl","value":"https://api.example.com","enabled":true,"secret":false},{"key":"token","value":"","enabled":true,"secret":true}]}}',
   1790000300000, 1790000300000, NULL),
  ('SeedShareW1Active00002', '01JSEEDWS00000000000000001', 'request', '01JSEEDENT0000000000000003',
   '{"schemaVersion":1,"request":{"id":"01JSEEDENT0000000000000003","name":"Create order ✓","method":"POST","url":"https://api.example.com/orders","params":[],"headers":[{"key":"Content-Type","value":"application/json","enabled":true}],"auth":{"type":"none"},"body":{"type":"json","text":"{\"sku\":\"A-1\",\"qty\":2}"},"settings":{},"sortOrder":0}}',
   1790000400000, 1790000500000, NULL),
  -- A collection without a name: the share list reports name null.
  ('SeedShareW1Active00003', '01JSEEDWS00000000000000001', 'collection', '01JSEEDENT0000000000000006',
   '{"schemaVersion":1,"collection":{"id":"01JSEEDENT0000000000000006","folders":[],"requests":[]}}',
   1790000400000, 1790000400000, NULL),
  ('SeedShareW1Revokd00001', '01JSEEDWS00000000000000001', 'collection', '01JSEEDENT0000000000000011', NULL, 1790000250000, 1790000260000, 1790000270000),
  ('SeedShareW1Revokd00002', '01JSEEDWS00000000000000001', 'request', '01JSEEDENT0000000000000013', NULL, 1790000280000, 1790000280000, 1790000290000),
  ('SeedShareW2Active00001', '01JSEEDWS00000000000000002', 'request', '01JSEEDENT0000000000000603',
   '{"schemaVersion":1,"request":{"id":"01JSEEDENT0000000000000603","name":"ワーク","method":"GET","url":"https://例え.jp/パス?q=値","params":[],"headers":[],"auth":{"type":"basic","username":"u","password":"{{pw}}"},"body":{"type":"none"},"settings":{},"sortOrder":0}}',
   1790000600000, 1790000600000, NULL),
  ('SeedShareW2Active00002', '01JSEEDWS00000000000000002', 'collection', '01JSEEDENT0000000000000606',
   '{"schemaVersion":1,"collection":{"id":"01JSEEDENT0000000000000606","name":"Grpc later","folders":[],"requests":[{"id":"G1","collectionId":"01JSEEDENT0000000000000606","folderId":null,"name":"Unary","kind":"grpc","sortOrder":0}]}}',
   1790000700000, 1790000700000, NULL),
  ('SeedShareW2Revokd00001', '01JSEEDWS00000000000000002', 'collection', '01JSEEDENT0000000000000611', NULL, 1790000610000, 1790000610000, 1790000620000),
  ('SeedShareW3Revokd00001', '01JSEEDWS00000000000000003', 'request', '01JSEEDENT0000000000000903', NULL, 1790000810000, 1790000810000, 1790000820000);
UPDATE shares SET size = CASE WHEN revoked_at IS NULL THEN COALESCE(length(CAST(snapshot AS BLOB)), 0) ELSE 0 END;

INSERT INTO rate_limits (key, window_start, count) VALUES ('ws_create:5f2b', 1790000000000, 3);
