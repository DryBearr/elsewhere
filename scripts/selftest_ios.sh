#!/usr/bin/env bash
# Checks the job path through the real iOS app on a booted simulator: the same steps as
# scripts/selftest_android.sh (kill halfway, Interrupted, Resume to Done, Discard to
# Cancelled, yt-dlp started lazily).
#
#   scripts/selftest_ios.sh <simulator udid> <Elsewhere.app built for the simulator>
set -euo pipefail

udid=$1
app=$2
bundle=io.github.drybearr.elsewhere
check=$(dirname "$0")/check_selftest.py

launch() {
  local step=$1
  shift
  if [ -n "${1:-}" ]; then
    xcrun simctl launch --console-pty "$udid" "$bundle" -selftest "$1" > "$step.log" 2>&1 &
  else
    xcrun simctl launch --console-pty "$udid" "$bundle" > "$step.log" 2>&1 &
  fi
}

stop() {
  xcrun simctl terminate "$udid" "$bundle" 2> /dev/null || true
  sleep 2
}

expect() {
  local step=$1 status
  shift
  for _ in $(seq 180); do
    status=0
    python3 "$check" "$step.log" "$@" || status=$?
    if [ "$status" -eq 0 ]; then
      echo "ok: $step ($*)"
      return 0
    fi
    [ "$status" -ge 2 ] && break
    sleep 1
  done
  echo "FAILED: $step ($*)"
  cat "$step.log"
  exit 1
}

xcrun simctl install "$udid" "$app"
launch 1-demo demo
expect 1-demo running 2
expect 1-demo revisions
stop
launch 2-restart
expect 2-restart interrupted
stop
launch 3-resume resume
expect 3-resume final done 19 1
expect 3-resume revisions
stop
launch 4-demo demo
expect 4-demo running 1
stop
launch 5-discard discard
expect 5-discard final cancelled
stop
launch 6-ytdlp ytdlp
expect 6-ytdlp ytdlp
stop
echo "The job path works in the iOS app."
