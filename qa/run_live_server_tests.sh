#!/usr/bin/env bash
# Live client suite against the Go server (docs/plans/accounts/06-go-server.md §7.5), the sibling
# of run_live_worker_tests.sh. Usage, from the repo root on the host:
#
#   docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server
#   docker compose run --rm dev bash qa/run_live_server_tests.sh
#
# Runs inside the dev container: starts server/bin/reqharbor-server-devauth (static, so it runs in
# the dev image) on loopback with a throwaway data dir, waits for /readyz, then runs
#   1. qa/worker_smoke.sh                          (curl smoke + security checks)
#   2. cargo test --test qa_sync_live -- --ignored  (today's Rust client <-> Go server, token sync)
#   3. cargo test --test qa_server_live -- --ignored (Go-server-only raw HTTP cases)
# then prints the server's error-level log lines and stops it. Exits non-zero if any step failed.
#
# The rate limit is NOT overridden: the legacy-create limit stays at the default 10 per hour.
# worker_smoke.sh sends CF-Connecting-IP, honoured because 127.0.0.1 is a trusted proxy, so its
# creates land in random 203.0.113.x / 192.0.2.x buckets; qa_sync_live's creates send no header
# and share the 127.0.0.1 bucket; qa_server_live sends random 198.18.0.0/15 addresses.
set -u
ROOT=${ROOT:-/app}
BIN="$ROOT/server/bin/reqharbor-server-devauth"
PORT=${PORT:-8080}
BASE="http://127.0.0.1:$PORT"

if [ ! -x "$BIN" ]; then
  echo "missing $BIN; build it first:" >&2
  echo "  docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server" >&2
  exit 2
fi

DATA=$(mktemp -d /tmp/qa-reqharbor-server-XXXX)
LOG="$DATA/server.log"
env -i PATH="$PATH" HOME="${HOME:-/root}" \
  REQHARBOR_LISTEN="127.0.0.1:$PORT" \
  REQHARBOR_PUBLIC_URL="$BASE" \
  REQHARBOR_DATA_DIR="$DATA/data" \
  REQHARBOR_DEV_AUTH=true \
  REQHARBOR_LEGACY_WORKSPACES=open \
  REQHARBOR_TRUSTED_PROXIES=127.0.0.1/32 \
  REQHARBOR_CLIENT_IP_HEADER=CF-Connecting-IP \
  REQHARBOR_LOG_FORMAT=json \
  "$BIN" serve >"$LOG" 2>&1 &
SPID=$!
cleanup() {
  kill "$SPID" 2>/dev/null
  wait "$SPID" 2>/dev/null
  rm -rf "$DATA"
}
trap cleanup EXIT

ready=""
for _ in $(seq 1 60); do
  if ! kill -0 "$SPID" 2>/dev/null; then
    echo "server exited during startup:" >&2
    cat "$LOG" >&2
    exit 2
  fi
  code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/readyz")
  if [ "$code" = 200 ]; then ready=1; break; fi
  sleep 0.5
done
if [ -z "$ready" ]; then
  echo "server not ready after 30 s:" >&2
  cat "$LOG" >&2
  exit 2
fi
echo "server ready: $(curl -s "$BASE/v1/server" | head -c 200)"

echo "=== curl smoke ==="
BASE="$BASE" bash "$ROOT/qa/worker_smoke.sh"
SMOKE=$?

cd "$ROOT" || exit 2
run_live() { # run_live <test target>
  echo "=== live Rust client <-> Go server: $1 ==="
  REQHARBOR_SERVER_URL="$BASE" REQHARBOR_WORKER_URL="$BASE" \
    cargo test -p reqharbor-core --test "$1" -- --ignored --test-threads=1 2>&1 \
    | grep -vE '^\s+(Compiling|Running|Finished)'
  return "${PIPESTATUS[0]}"
}
run_live qa_sync_live
SYNC=$?
run_live qa_server_live
SERVER=$?

echo "=== server log (error level) ==="
grep -E '"level":"ERROR"' "$LOG" | head -20
echo "smoke exit=$SMOKE qa_sync_live exit=$SYNC qa_server_live exit=$SERVER"
[ "$SMOKE" -eq 0 ] && [ "$SYNC" -eq 0 ] && [ "$SERVER" -eq 0 ]
