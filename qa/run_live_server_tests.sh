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
#   3. cargo test --test qa_server_live -- --ignored (Go-server-only raw HTTP cases, including the S4
#      accounts case: it signs up the first account with the setup code that `serve` logs, which this
#      script reads from the log and passes as REQHARBOR_LIVE_SIGNUP_CODE)
#   4. cargo test --test qa_auth_live -- --ignored  (the client's sign-in, M1-client C1; §7.5 step 6)
#      with REQHARBOR_LIVE_SIGNUP_CODE set to a multi-use invite made with `invite create`. It needs
#      the S6 routes (GET /v1/me, sessions, personal spaces; §9, §10 S6), so by default
#      (QA_AUTH_LIVE=auto) it runs only when the running server serves them: an unauthenticated
#      GET /v1/me answers 401 (route present) rather than 404. Otherwise it prints
#      "qa_auth_live skipped: server does not serve /v1/me yet (S6)". QA_AUTH_LIVE=1 forces the run,
#      QA_AUTH_LIVE=0 skips it.
#   5. node qa/server_pow_check.js                   (the login proof of work, pow.js, without WebCrypto
#      as on an http PUBLIC_URL that is not loopback; needs no server)
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
# Exactly the §7.5 settings, for serve and for the CLI next to it (rh <command>).
RH_ENV=(
  PATH="$PATH" HOME="${HOME:-/root}"
  REQHARBOR_LISTEN="127.0.0.1:$PORT"
  REQHARBOR_PUBLIC_URL="$BASE"
  REQHARBOR_DATA_DIR="$DATA/data"
  REQHARBOR_DEV_AUTH=true
  REQHARBOR_LEGACY_WORKSPACES=open
  REQHARBOR_TRUSTED_PROXIES=127.0.0.1/32
  REQHARBOR_CLIENT_IP_HEADER=CF-Connecting-IP
  REQHARBOR_LOG_FORMAT=json
)
rh() { env -i "${RH_ENV[@]}" "$BIN" "$@"; }
env -i "${RH_ENV[@]}" "$BIN" serve >"$LOG" 2>&1 &
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
SETUP_CODE=$(sed -n 's/.*enter setup code \([0-9A-Z]\{4\}-[0-9A-Z]\{4\}-[0-9A-Z]\{4\}-[0-9A-Z]\{4\}\).*/\1/p' "$LOG" | head -1)
if [ -z "$SETUP_CODE" ]; then
  echo "no setup code in the server log:" >&2
  cat "$LOG" >&2
  exit 2
fi

echo "=== curl smoke ==="
BASE="$BASE" bash "$ROOT/qa/worker_smoke.sh"
SMOKE=$?

cd "$ROOT" || exit 2
run_live() { # run_live <test target> [signup code]
  echo "=== live Rust client <-> Go server: $1 ==="
  REQHARBOR_SERVER_URL="$BASE" REQHARBOR_WORKER_URL="$BASE" REQHARBOR_LIVE_SIGNUP_CODE="${2:-}" \
    cargo test -p reqharbor-core --test "$1" -- --ignored --test-threads=1 2>&1 \
    | grep -vE '^\s+(Compiling|Running|Finished)'
  return "${PIPESTATUS[0]}"
}
run_live qa_sync_live
SYNC=$?
run_live qa_server_live "$SETUP_CODE"
SERVER=$?

AUTH=skipped
want_auth=${QA_AUTH_LIVE:-auto}
if [ "$want_auth" = auto ]; then
  # Decide on what the server serves, not on git state: 401 means the route exists and wants a
  # token; 404 means this server phase does not have it yet.
  me_code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/v1/me")
  if [ "$me_code" = 401 ]; then
    want_auth=1
  else
    want_auth=0
    echo "qa_auth_live skipped: server does not serve /v1/me yet (S6) (GET /v1/me -> $me_code)"
  fi
fi
if [ "$want_auth" = 1 ]; then
  # The setup code is spent once qa_server_live created the first account; later signups need
  # an invite. The CLI works next to the running server (no lock, short transactions).
  INVITE=$(rh invite create --uses 50 --ttl 1h | head -1)
  run_live qa_auth_live "$INVITE"
  AUTH=$?
fi

echo "=== pow.js without WebCrypto ==="
node "$ROOT/qa/server_pow_check.js"
POW=$?

echo "=== server log (error level) ==="
grep -E '"level":"ERROR"' "$LOG" | head -20
echo "smoke exit=$SMOKE qa_sync_live exit=$SYNC qa_server_live exit=$SERVER qa_auth_live exit=$AUTH pow exit=$POW"
[ "$SMOKE" -eq 0 ] && [ "$SYNC" -eq 0 ] && [ "$SERVER" -eq 0 ] && [ "$POW" -eq 0 ] && { [ "$AUTH" = skipped ] || [ "$AUTH" -eq 0 ]; }
