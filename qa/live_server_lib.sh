# Shared by qa/run_live_server_tests.sh and qa/live_server_ctl.sh (source it; needs bash).
# Inputs: ROOT (repo root, default /app), QA_LIVE_DATA (the throwaway directory), PORT (8080).
# Defines BIN, BASE, LOG, PIDFILE, RH_ENV and the functions rh, server_start, server_stop and
# server_wait_ready. The server's own data dir is $QA_LIVE_DATA/data.
ROOT=${ROOT:-/app}
BIN="$ROOT/server/bin/reqharbor-server-devauth"
PORT=${PORT:-8080}
BASE="http://127.0.0.1:$PORT"
LOG="$QA_LIVE_DATA/server.log"
PIDFILE="$QA_LIVE_DATA/server.pid"
# Exactly the §7.5 settings, for serve and for the CLI next to it (rh <command>).
RH_ENV=(
  PATH="$PATH" HOME="${HOME:-/root}"
  REQHARBOR_LISTEN="127.0.0.1:$PORT"
  REQHARBOR_PUBLIC_URL="$BASE"
  REQHARBOR_DATA_DIR="$QA_LIVE_DATA/data"
  REQHARBOR_DEV_AUTH=true
  REQHARBOR_LEGACY_WORKSPACES=open
  REQHARBOR_TRUSTED_PROXIES=127.0.0.1/32
  REQHARBOR_CLIENT_IP_HEADER=CF-Connecting-IP
  REQHARBOR_LOG_FORMAT=json
)
rh() { env -i "${RH_ENV[@]}" "$BIN" "$@"; }

# server_start: serve in the background, appending to $LOG; the pid goes to $PIDFILE.
server_start() {
  env -i "${RH_ENV[@]}" "$BIN" serve >>"$LOG" 2>&1 &
  echo $! >"$PIDFILE"
}

# server_alive PID: true while the process runs (a zombie counts as gone).
server_alive() {
  local state
  state=$(awk '{print $3}' "/proc/$1/stat" 2>/dev/null) || return 1
  [ -n "$state" ] && [ "$state" != Z ]
}

# server_stop: SIGTERM (graceful shutdown), then wait up to 30 s for the process to exit.
server_stop() {
  local pid
  pid=$(cat "$PIDFILE" 2>/dev/null) || return 0
  [ -n "$pid" ] || return 0
  kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 1 300); do
    server_alive "$pid" || { rm -f "$PIDFILE"; return 0; }
    sleep 0.1
  done
  echo "server $pid did not stop after SIGTERM; killing it" >&2
  kill -KILL "$pid" 2>/dev/null
  rm -f "$PIDFILE"
  return 1
}

# server_wait_ready: wait up to 30 s for /readyz 200; prints the log and fails otherwise.
server_wait_ready() {
  local pid code
  pid=$(cat "$PIDFILE" 2>/dev/null)
  for _ in $(seq 1 60); do
    if [ -z "$pid" ] || ! server_alive "$pid"; then
      echo "server exited during startup:" >&2
      tail -50 "$LOG" >&2
      return 2
    fi
    code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/readyz")
    [ "$code" = 200 ] && return 0
    sleep 0.5
  done
  echo "server not ready after 30 s:" >&2
  tail -50 "$LOG" >&2
  return 2
}
