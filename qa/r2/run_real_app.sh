#!/usr/bin/env bash
# QA round 2 — run the REAL ReqHarbor app (release build, custom-protocol) under Xvfb and drive it
# with tauri-driver + WebKitWebDriver. Usage (host, repo root):
#   docker compose run --rm dev bash qa/r2/run_real_app.sh
# Prereqs (one-off, cached in the `target` volume):
#   cargo build --release -p reqharbor-app --features tauri/custom-protocol
#   cargo install tauri-driver --locked --root /cache/target/tools
# Installs webkit2gtk-driver into this throwaway container (not in the image yet).
set -u
OUT=/app/qa/r2/out
mkdir -p "$OUT"
export APP=${APP:-/cache/target/release/reqharbor}
export ECHO=http://127.0.0.1:8099
export OUT

command -v WebKitWebDriver || { echo "WebKitWebDriver not available"; exit 3; }

# fresh app data dir for this run
export XDG_DATA_HOME=$(mktemp -d /tmp/qa-xdg-XXXX)
export XDG_CONFIG_HOME=$XDG_DATA_HOME/config
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/qa-run-XXXX); chmod 700 "$XDG_RUNTIME_DIR"

Xvfb :99 -screen 0 1440x900x24 >/tmp/xvfb.log 2>&1 &
export DISPLAY=:99
node /app/qa/r2/echo_server.mjs 8099 >/tmp/echo.log 2>&1 &

if [ "${WITH_WORKER:-1}" = 1 ]; then
  PERSIST=$(mktemp -d /tmp/qa-wrangler-XXXX)
  (cd /app/worker && npx wrangler d1 migrations apply DB --local --persist-to "$PERSIST" >/dev/null 2>&1 \
     && npx wrangler dev --ip 127.0.0.1 --port 8787 --persist-to "$PERSIST" >/tmp/wrangler.log 2>&1) &
  for _ in $(seq 1 60); do curl -s -o /dev/null http://127.0.0.1:8787/ && break; sleep 1; done
  export WORKER=http://127.0.0.1:8787
fi
sleep 1

# Session D-Bus (tauri-plugin-single-instance uses it on Linux)
if [ "${NO_DBUS:-0}" != 1 ]; then
  export DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --session --fork --print-address=1 2>/dev/null | head -1)
fi
echo "DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-<none>}"

$(command -v tauri-driver || echo /cache/target/tools/bin/tauri-driver) --port 4444 --native-driver "$(command -v WebKitWebDriver)" >/tmp/tauri-driver.log 2>&1 &
for _ in $(seq 1 30); do curl -s http://127.0.0.1:4444/status >/dev/null && break; sleep 0.5; done

node /app/qa/r2/real_app_smoke.mjs 2>&1 | tee "$OUT/real_app_smoke.log"
RC=${PIPESTATUS[0]}
echo "--- tauri-driver log (tail)"; tail -30 /tmp/tauri-driver.log
echo "--- app data dir"; find "$XDG_DATA_HOME" -maxdepth 3 -type f | head -20
cp -r "$XDG_DATA_HOME" "$OUT/appdata" 2>/dev/null || true
exit $RC
