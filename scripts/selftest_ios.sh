#!/usr/bin/env bash
# Checks the job path through the real iOS app on a booted simulator: the same steps as
# scripts/selftest_android.sh (kill halfway, Interrupted, Resume to Done, Discard to
# Cancelled, yt-dlp started lazily, the media engine), then background playback: it plays
# on behind another app, and the lock screen's buttons, interruptions (calls), unplugged
# headphones and a media services reset control it (SelfTest.swift posts them as iOS does).
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

# act <action>: a self-test action for the running app, by Darwin notification; the next
# expect_new looks at what comes after it. send: the same, without moving that mark.
act() {
  mark=$(wc -l < "$log")
  send "$1"
}

send() {
  xcrun simctl spawn "$udid" notifyutil -p "io.github.drybearr.elsewhere.selftest.$1"
}

# expect_new <step> <check...>: like expect, on the lines logged since the last act.
expect_new() {
  local step=$1 status
  shift
  for _ in $(seq 60); do
    tail -n "+$((mark + 1))" "$log" > "$step.log"
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
launch 4-demo long-demo
expect 4-demo running 1
stop
launch 5-discard discard
expect 5-discard final cancelled
stop
launch 6-ytdlp ytdlp
expect 6-ytdlp ytdlp
stop
launch 7-player player
expect 7-player player song.m4a clip.webm
stop

# 8. Background playback. The test songs are 6 s and 2 s long, on repeat.
log=8-background.log
mark=0
launch 8-background background
expect_new 8-play queue status=playing wants=true title=long
act report
expect_new 8-play nowplaying shown=true rate=1.0 index=0 next=true title=long
# Another app comes to the front: playback goes on behind it.
mark=$(wc -l < "$log")
xcrun simctl launch "$udid" com.apple.Preferences > /dev/null
expect_new 8-behind queue-advancing 3000
act remote-pause
expect_new 8-remote-pause queue status=paused wants=false
act report
expect_new 8-remote-pause nowplaying shown=true rate=0.0
# Back to the front (the simulator cannot wake a suspended app the way the lock screen can).
xcrun simctl launch "$udid" "$bundle" > /dev/null
act report
sleep 1
send remote-next
expect_new 8-remote-next queue-moved
act report
sleep 1
send remote-previous
expect_new 8-remote-previous queue-moved
act remote-toggle
expect_new 8-remote-toggle queue status=playing wants=true

# A call: paused, and playing again after it.
act interruption-began
expect_new 8-call queue status=paused wants=false keeps=true
act interruption-ended
expect_new 8-call-over queue status=playing wants=true
# Paused by the person during a call: it stays paused after the call.
act interruption-began
expect_new 8-call-pause queue wants=false keeps=true
act remote-pause
expect_new 8-call-pause queue keeps=false
act interruption-ended
sleep 3
expect_new 8-call-pause-over queue-absent wants=true
act remote-play
expect_new 8-call-pause-play queue status=playing wants=true

# Headphones unplugged.
act route-unplugged
expect_new 8-unplugged queue status=paused wants=false
act remote-play
expect_new 8-unplugged-play queue status=playing wants=true

# iOS restarts its media services: the sound comes back and plays on.
act media-reset
expect_new 8-media-reset queue-advancing 2000

# Stop: the queue empties and it leaves the lock screen.
act remote-stop
expect_new 8-stop queue entries=0
act report
expect_new 8-stop nowplaying shown=false
stop
echo "The job path works in the iOS app."
