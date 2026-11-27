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
#      script reads from the log and passes as REQHARBOR_LIVE_SIGNUP_CODE; and the S5 dev-provider
#      case, which signs up through the dev form with an invite made here with `invite create` and
#      passed as REQHARBOR_LIVE_INVITE_CODE; and the S6 personal-spaces case, which signs two more
#      accounts up with the same invite; and the S7 restore cases, which stop the server, restore a
#      backup with `restore` (or copy it over by hand) and start it again through
#      qa/live_server_ctl.sh, passed as REQHARBOR_LIVE_SERVER_CTL; and the M2 teams case, which
#      signs two more accounts up with the invite (so it has 10 uses) and a third with a team-invite
#      signup ticket. The server runs with REQHARBOR_TEAMS=true for it (qa/live_server_lib.sh))
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
# The server start, stop and readiness helpers live in qa/live_server_lib.sh, which
# qa/live_server_ctl.sh shares (the restore cases restart the server through it).
set -u
ROOT=${ROOT:-/app}
QA_LIVE_DATA=$(mktemp -d /tmp/qa-reqharbor-server-XXXX)
export QA_LIVE_DATA
# shellcheck source=qa/live_server_lib.sh
. "$ROOT/qa/live_server_lib.sh"

if [ ! -x "$BIN" ]; then
  echo "missing $BIN; build it first:" >&2
  echo "  docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server" >&2
  rm -rf "$QA_LIVE_DATA"
  exit 2
fi

server_start
cleanup() {
  server_stop >/dev/null 2>&1
  rm -rf "$QA_LIVE_DATA"
}
trap cleanup EXIT

server_wait_ready || exit 2
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
run_live() { # run_live <test target> [signup code] [invite code]
  echo "=== live Rust client <-> Go server: $1 ==="
  REQHARBOR_SERVER_URL="$BASE" REQHARBOR_WORKER_URL="$BASE" REQHARBOR_LIVE_SIGNUP_CODE="${2:-}" \
    REQHARBOR_LIVE_INVITE_CODE="${3:-}" REQHARBOR_LIVE_SERVER_CTL="$ROOT/qa/live_server_ctl.sh" \
    cargo test -p reqharbor-core --test "$1" -- --ignored --test-threads=1 2>&1 \
    | grep -vE '^\s+(Compiling|Running|Finished)'
  return "${PIPESTATUS[0]}"
}
run_live qa_sync_live
SYNC=$?
# The dev-provider case needs an invite. The CLI works next to the running server (no lock, short
# transactions); the invite is consumed only after the setup-code case created the first account.
LIVE_INVITE=$(rh invite create --uses 10 --ttl 1h | head -1)
if [ -z "$LIVE_INVITE" ]; then
  echo "invite create printed no code" >&2
  exit 2
fi
run_live qa_server_live "$SETUP_CODE" "$LIVE_INVITE"
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
