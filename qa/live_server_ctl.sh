#!/usr/bin/env bash
# Server control for the live tests that need to stop and restart the server (the S7 restore cases
# in crates/core/tests/qa_server_live.rs). qa/run_live_server_tests.sh passes this script's path as
# REQHARBOR_LIVE_SERVER_CTL and the run's directory as QA_LIVE_DATA; the tests run
#   bash "$REQHARBOR_LIVE_SERVER_CTL" <command> [FILE]
# Commands (results on stdout, diagnostics on stderr, non-zero exit on failure):
#   backup        reqharbor-server backup into the data dir's backups/; prints the backup's path
#   restore FILE  stop the server, reqharbor-server restore FILE, start it, wait for /readyz
#   replace FILE  the unsupported by-hand path: stop, copy FILE over reqharbor.db (dropping its
#                 -wal and -shm), start, wait for /readyz
set -u
: "${QA_LIVE_DATA:?QA_LIVE_DATA is not set (run qa/run_live_server_tests.sh)}"
# shellcheck source=qa/live_server_lib.sh
. "$(dirname "$0")/live_server_lib.sh"

cmd=${1:-}
case "$cmd" in
  backup)
    out="$QA_LIVE_DATA/data/backups/live-$(date +%Y%m%d-%H%M%S)-$$.db"
    rh backup --out "$out" >&2 || exit 1
    echo "$out"
    ;;
  restore)
    file=${2:?usage: live_server_ctl.sh restore FILE}
    server_stop || exit 1
    rh restore "$file" >&2 || { server_start; server_wait_ready; exit 1; }
    server_start
    server_wait_ready || exit 1
    ;;
  replace)
    file=${2:?usage: live_server_ctl.sh replace FILE}
    server_stop || exit 1
    db="$QA_LIVE_DATA/data/reqharbor.db"
    cp "$file" "$db" && rm -f "$db-wal" "$db-shm" || exit 1
    server_start
    server_wait_ready || exit 1
    ;;
  *)
    echo "usage: live_server_ctl.sh backup | restore FILE | replace FILE" >&2
    exit 2
    ;;
esac
