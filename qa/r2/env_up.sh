#!/usr/bin/env bash
# QA round 2 — bring up Xvfb, D-Bus, echo server, (wrangler) and tauri-driver inside a long-lived
# container (docker compose run -d --name qa-r2-app dev sleep infinity; docker exec qa-r2-app bash qa/r2/env_up.sh).
# Writes the environment to /tmp/qa-env.sh for later `docker exec ... bash -c '. /tmp/qa-env.sh; ...'`.
set -u
pkill -f tauri-driver; pkill -f WebKitWebDriver; pkill -f release/reqharbor; sleep 0.5
D=${QA_DATA:-/tmp/qa-xdg}
mkdir -p "$D" /tmp/qa-run && chmod 700 /tmp/qa-run
pgrep Xvfb >/dev/null || (Xvfb :99 -screen 0 1440x900x24 >/tmp/xvfb.log 2>&1 &)
pgrep -f echo_server.mjs >/dev/null || (node /app/qa/r2/echo_server.mjs 8099 >/tmp/echo.log 2>&1 &)
if [ ! -f /tmp/dbus.addr ]; then dbus-daemon --session --fork --print-address=1 | head -1 > /tmp/dbus.addr; fi
if [ "${WITH_WORKER:-1}" = 1 ] && ! curl -s -o /dev/null http://127.0.0.1:8787/; then
  P=$(mktemp -d /tmp/qa-wrangler-XXXX)
  (cd /app/worker && npx wrangler d1 migrations apply DB --local --persist-to "$P" >/dev/null 2>&1 && setsid npx wrangler dev --ip 127.0.0.1 --port 8787 --persist-to "$P" >/tmp/wrangler.log 2>&1 &)
  for _ in $(seq 1 60); do curl -s -o /dev/null http://127.0.0.1:8787/ && break; sleep 1; done
fi
cat > /tmp/qa-env.sh <<EOT
export DISPLAY=:99 XDG_DATA_HOME=$D XDG_CONFIG_HOME=$D/config XDG_RUNTIME_DIR=/tmp/qa-run
export DBUS_SESSION_BUS_ADDRESS=$(cat /tmp/dbus.addr)
export APP=\${APP:-/cache/target/release/reqharbor} ECHO=http://127.0.0.1:8099 WORKER=http://127.0.0.1:8787 OUT=/app/qa/r2/out
EOT
. /tmp/qa-env.sh
(setsid $(command -v tauri-driver || echo /cache/target/tools/bin/tauri-driver) --port 4444 --native-driver /usr/bin/WebKitWebDriver >/tmp/td.log 2>&1 &)
for _ in $(seq 1 30); do curl -s http://127.0.0.1:4444/status >/dev/null && break; sleep 0.5; done
echo "env up: data=$D worker=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8787/) echo=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8099/echo)"
