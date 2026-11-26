#!/usr/bin/env bash
# QA round 1 (updated round 2) — live Worker smoke test with curl (runs INSIDE the dev container,
# against `wrangler dev` on $BASE). Started by qa/run_live_worker_tests.sh.
# Prints PASS/FAIL per check and exits non-zero if any check failed.
set -u
BASE=${BASE:-http://127.0.0.1:8787}
FAILS=0
T=$(mktemp -d)

pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; FAILS=$((FAILS + 1)); }
check() { # check <name> <expected> <actual>
  if [ "$2" = "$3" ]; then pass "$1 ($3)"; else fail "$1: expected '$2' got '$3'"; fi
}
# jget <file> <js expression over `d`>
jget() { node -e 'const d=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));const v=('"$2"');process.stdout.write(typeof v==="string"?v:JSON.stringify(v))' "$1"; }
# req <method> <path> <outfile> [curl args...] -> prints status
req() { local m=$1 p=$2 o=$3; shift 3; curl -s -o "$o" -D "$o.h" -w '%{http_code}' -X "$m" "$BASE$p" "$@"; }
hdr() { grep -i "^$2:" "$1.h" | head -1 | cut -d' ' -f2- | tr -d '\r'; }

IP="203.0.113.$((RANDOM % 200 + 1))"
J=(-H 'Content-Type: application/json')

# 1. create workspace
s=$(req POST /v1/workspaces $T/ws "${J[@]}" -H "CF-Connecting-IP: $IP" -d '{"name":"QA smoke"}')
check "create workspace -> 201" 201 "$s"
TOKEN=$(jget $T/ws d.token); WSID=$(jget $T/ws d.workspaceId)
[[ $TOKEN =~ ^rh_[A-Za-z0-9_-]{43}$ ]] && pass "token format rh_+43 base64url" || fail "token format: $TOKEN"
A=(-H "Authorization: Bearer $TOKEN")

s=$(req GET /v1/workspace $T/w "${A[@]}"); check "GET /v1/workspace" 200 "$s"
check "workspace id round-trips" "$WSID" "$(jget $T/w d.workspaceId)"

# 2. auth failures
s=$(req GET /v1/workspace $T/x -H 'Authorization: Bearer rh_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'); check "unknown token -> 401" 401 "$s"
check "401 has WWW-Authenticate" 'Bearer realm="reqharbor"' "$(hdr $T/x WWW-Authenticate)"
s=$(req GET /v1/workspace $T/x -H "Authorization: Basic $TOKEN"); check "Basic scheme -> 401" 401 "$s"
s=$(req GET /v1/workspace $T/x); check "no token -> 401" 401 "$s"
check "401 JSON error code" unauthorized "$(jget $T/x d.error.code)"

# 3. push / pull
NOW=$(date +%s%3N)
cat > $T/push.json <<EOF
{"changes":[
 {"type":"project","id":"01PROJ","updatedAt":$NOW,"deleted":false,"data":{"id":"01PROJ","name":"P"}},
 {"type":"environment","id":"01ENV","updatedAt":$NOW,"deleted":false,"data":{"id":"01ENV","name":"E","variables":[{"key":"tok","value":"LEAKME","enabled":true,"secret":true}]}}
]}
EOF
s=$(req POST /v1/sync $T/p "${A[@]}" "${J[@]}" --data-binary @$T/push.json); check "push" 200 "$s"
check "push results applied" '["applied","applied"]' "$(jget $T/p 'd.results.map(r=>r.status)')"
s=$(req GET '/v1/sync?since=0&limit=500' $T/pull "${A[@]}"); check "pull" 200 "$s"
check "pull returns 2 changes" 2 "$(jget $T/pull d.changes.length)"
grep -q LEAKME $T/pull && fail "secret value stored by sync" || pass "secret env value not stored"
OLD=$((NOW - 1000))
s=$(req POST /v1/sync $T/p "${A[@]}" "${J[@]}" -d "{\"changes\":[{\"type\":\"project\",\"id\":\"01PROJ\",\"updatedAt\":$OLD,\"deleted\":false,\"data\":{\"name\":\"old\"}}]}")
check "older push -> stale" stale "$(jget $T/p 'd.results[0].status')"
s=$(req POST /v1/sync $T/p "${A[@]}" "${J[@]}" -d '{"changes":[{"type":"history","id":"h1","updatedAt":1,"deleted":false,"data":{}}]}')
check "history type rejected" 400 "$s"

# 4. shares (with XSS payloads)
XSS='\"><img src=x onerror=alert(1)><script>alert(document.domain)</script>'
cat > $T/share.json <<EOF
{"kind":"collection","entityId":"01COL","snapshot":{"schemaVersion":1,
 "collection":{"id":"01COL","name":"$XSS","description":"$XSS","auth":{"type":"bearer","token":"{{token}}"},
  "folders":[{"id":"F1","name":"$XSS","parentFolderId":null,"sortOrder":0}],
  "requests":[{"id":"R1","folderId":"F1","name":"$XSS","method":"QUERY","url":"https://api.x.dev/s?q=$XSS",
    "params":[],"headers":[{"key":"X-A","value":"$XSS","enabled":true}],"auth":{"type":"inherit"},
    "body":{"type":"json","text":"{\\"a\\":\\"</pre><script>alert(1)</script>\\"}"},
    "settings":{"followRedirects":true,"timeoutMs":30000,"verifyTls":true},"sortOrder":0}]},
 "environment":{"name":"$XSS","variables":[{"key":"token","value":"SHARE-SECRET","enabled":true,"secret":true},{"key":"baseUrl","value":"https://x","enabled":true,"secret":false}]}}}
EOF
s=$(req POST /v1/shares $T/sh "${A[@]}" "${J[@]}" --data-binary @$T/share.json); check "create share" 201 "$s"
SID=$(jget $T/sh d.shareId); SURL=$(jget $T/sh d.url)
[[ $SID =~ ^[0-9A-Za-z]{22}$ ]] && pass "share id 22-char base62" || fail "share id: $SID"
s=$(req GET /s/$SID $T/page); check "GET /s/:id HTML" 200 "$s"
grep -q '<img src=x' $T/page && fail "XSS: raw <img> rendered" || pass "XSS <img> escaped"
grep -q '<script>alert' $T/page && fail "XSS: raw <script> rendered" || pass "XSS <script> escaped"
grep -q '&lt;script&gt;alert' $T/page && pass "payload present escaped" || fail "escaped payload missing"
grep -q SHARE-SECRET $T/page && fail "secret variable rendered on share page" || pass "secret var removed from page"
CSP=$(hdr $T/page Content-Security-Policy)
[[ $CSP == *"default-src 'none'"* && $CSP == *"script-src 'sha256-"* && $CSP != *unsafe-inline* ]] && pass "strict CSP" || fail "CSP: $CSP"
check "X-Robots-Tag" "noindex, nofollow" "$(hdr $T/page X-Robots-Tag)"
check "Cache-Control public 60" "public, max-age=60" "$(hdr $T/page Cache-Control)"
[ -z "$(hdr $T/page Set-Cookie)" ] && pass "no Set-Cookie" || fail "Set-Cookie present"
check "X-Frame-Options" DENY "$(hdr $T/page X-Frame-Options)"
s=$(req GET /s/$SID.json $T/sj); check "GET /s/:id.json" 200 "$s"
check "json CORS *" '*' "$(hdr $T/sj Access-Control-Allow-Origin)"
grep -q SHARE-SECRET $T/sj && fail "secret variable in share JSON" || pass "secret var removed from JSON"
s=$(req GET /s/$SID.json $T/sj -H "Origin: https://evil.example"); check "json readable cross-origin" 200 "$s"

# update keeps URL
sed 's/"QUERY"/"PATCH"/' $T/share.json | node -e 'let s="";process.stdin.on("data",c=>s+=c).on("end",()=>{const j=JSON.parse(s);process.stdout.write(JSON.stringify({snapshot:j.snapshot}))})' > $T/upd.json
s=$(req PUT /v1/shares/$SID $T/u "${A[@]}" "${J[@]}" --data-binary @$T/upd.json); check "update share" 200 "$s"
check "update keeps URL" "$SURL" "$(jget $T/u d.url)"
req GET /s/$SID.json $T/sj >/dev/null
check "update visible" PATCH "$(jget $T/sj 'd.snapshot.collection.requests[0].method')"
s=$(req GET /v1/shares $T/l "${A[@]}"); check "list shares" 1 "$(jget $T/l d.shares.length)"

# other workspace cannot touch it
s=$(req POST /v1/workspaces $T/ws2 "${J[@]}" -H "CF-Connecting-IP: $IP" -d '{"name":"QA other"}')
T2=$(jget $T/ws2 d.token)
s=$(req DELETE /v1/shares/$SID $T/x -H "Authorization: Bearer $T2"); check "other workspace revoke -> 404" 404 "$s"
s=$(req GET '/v1/sync?since=0' $T/x -H "Authorization: Bearer $T2"); check "other workspace sees no changes" 0 "$(jget $T/x d.changes.length)"

# revoke -> 410
s=$(req DELETE /v1/shares/$SID $T/x "${A[@]}"); check "revoke" 200 "$s"
s=$(req GET /s/$SID $T/x); check "revoked HTML -> 410" 410 "$s"
s=$(req GET /s/$SID.json $T/x); check "revoked JSON -> 410" 410 "$s"
s=$(req PUT /v1/shares/$SID $T/x "${A[@]}" "${J[@]}" --data-binary @$T/upd.json); check "update revoked -> 410" 410 "$s"
s=$(req DELETE /v1/shares/$SID $T/x "${A[@]}"); check "revoke idempotent" 200 "$s"

# 5. size limits
node -e 'process.stdout.write(JSON.stringify({changes:[{type:"request",id:"BIG",updatedAt:1,deleted:false,data:{b:"x".repeat(6*1024*1024)}}]}))' > $T/big.json
s=$(req POST /v1/sync $T/x "${A[@]}" "${J[@]}" --data-binary @$T/big.json); check "6 MB body -> 413" 413 "$s"
s=$(req POST /v1/sync $T/x "${A[@]}" "${J[@]}" -H 'Transfer-Encoding: chunked' --data-binary @$T/big.json); check "6 MB chunked -> 413" 413 "$s"
node -e 'process.stdout.write(JSON.stringify({changes:[{type:"request",id:"E",updatedAt:1,deleted:false,data:{b:"x".repeat(600*1024)}}]}))' > $T/ent.json
# Contract C2 (QA round 2): an oversized entity no longer fails the batch; the Worker answers 200
# with a per-change `rejected` result (reason entity_too_large) and stores nothing for it.
s=$(req POST /v1/sync $T/x "${A[@]}" "${J[@]}" --data-binary @$T/ent.json); check "entity > 512 KB -> 200 (per-change result)" 200 "$s"
check "entity > 512 KB -> result rejected" rejected "$(jget $T/x 'd.results[0].status')"
check "entity > 512 KB -> reason entity_too_large" entity_too_large "$(jget $T/x 'd.results[0].reason')"
node -e 'process.stdout.write(JSON.stringify({kind:"request",entityId:"R",snapshot:{schemaVersion:1,request:{name:"x".repeat(1100*1024)}}}))' > $T/snap.json
s=$(req POST /v1/shares $T/x "${A[@]}" "${J[@]}" --data-binary @$T/snap.json); check "snapshot > 1 MB -> 413" 413 "$s"
s=$(req POST /v1/sync $T/x "${A[@]}" -H 'Content-Type: text/plain' -d '{}'); check "wrong content-type -> 415" 415 "$s"
s=$(req POST /v1/sync $T/x "${A[@]}" "${J[@]}" -d '{"changes":'); check "invalid JSON -> 400" 400 "$s"

# 6. routing / misc
s=$(req GET /nope $T/x); check "unknown route -> 404" 404 "$s"
s=$(req PATCH /v1/sync $T/x "${A[@]}"); check "wrong method -> 405" 405 "$s"
check "405 Allow header" "GET, POST" "$(hdr $T/x Allow)"
s=$(req GET "/s/%27%20OR%201=1--" $T/x); check "SQLi-ish share id -> 404" 404 "$s"
s=$(req GET '/v1/sync?since=1%20OR%201=1' $T/x "${A[@]}"); check "SQLi-ish since -> 400" 400 "$s"

# 7. rate limit: 10 creates / hour / IP (this IP already used 2)
RL_IP="192.0.2.$((RANDOM % 200 + 1))"
codes=""
for i in $(seq 1 11); do
  codes+="$(req POST /v1/workspaces $T/x "${J[@]}" -H "CF-Connecting-IP: $RL_IP" -d '{"name":"rl"}') "
done
[[ $codes == *"429"* ]] && pass "rate limit hit within 11 creates ($codes)" || fail "no 429 after 11 creates: $codes"
[ -n "$(hdr $T/x Retry-After)" ] && pass "429 has Retry-After" || fail "429 missing Retry-After"
s=$(req POST /v1/workspaces $T/x "${J[@]}" -d '{"name":"no-ip"}')
echo "INFO  create without CF-Connecting-IP -> $s (all such clients share one 'unknown' bucket)"

echo "----"
echo "smoke failures: $FAILS"
rm -rf "$T"
exit $FAILS
