#!/usr/bin/env bash
# QA round 1 — start the Worker locally and run the live tests against it.
# Usage (from the repo root, on the host):
#   docker compose run --rm dev bash qa/run_live_worker_tests.sh
# Runs entirely inside one container: applies D1 migrations to a throwaway
# persist dir, starts `wrangler dev` in the background, runs
#   1. qa/worker_smoke.sh (curl smoke + security checks)
#   2. cargo test --test qa_sync_live -- --ignored  (real Rust client <-> real Worker)
# then stops wrangler.
set -u
PERSIST=$(mktemp -d /tmp/qa-wrangler-XXXX)
LOG=/tmp/qa-wrangler.log
cd /app/worker
[ -d node_modules/wrangler ] || npm ci --no-audit --no-fund
npx wrangler d1 migrations apply DB --local --persist-to "$PERSIST" 2>&1 | tail -3
npx wrangler dev --ip 0.0.0.0 --port 8787 --persist-to "$PERSIST" >"$LOG" 2>&1 &
WPID=$!
trap 'kill $WPID 2>/dev/null; wait $WPID 2>/dev/null' EXIT
for _ in $(seq 1 60); do
  curl -s -o /dev/null http://127.0.0.1:8787/ && break
  sleep 1
done
curl -s -o /dev/null -w 'worker up: %{http_code}\n' http://127.0.0.1:8787/

echo "=== curl smoke ==="
BASE=http://127.0.0.1:8787 bash /app/qa/worker_smoke.sh
SMOKE=$?

echo "=== live Rust client <-> Worker ==="
cd /app
REQHARBOR_WORKER_URL=http://127.0.0.1:8787 cargo test -p reqharbor-core --test qa_sync_live -- --ignored --test-threads=1 2>&1 \
  | grep -vE '^\s+(Compiling|Running|Finished)'
LIVE=${PIPESTATUS[0]}

echo "=== wrangler log (errors) ==="
grep -iE "error|exception" "$LOG" | grep -v "^\s*$" | head -20
echo "smoke exit=$SMOKE live exit=$LIVE"
