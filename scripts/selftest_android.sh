#!/usr/bin/env bash
# Checks the job path through the real Android app on an emulator (adb connected):
#   1. a demo job is killed halfway, 2. comes back Interrupted, 3. Resume ends it Done
#   with exact counts and the screen equal to the core; 4-5. another is discarded and
#   ends Cancelled; 6. yt-dlp starts lazily inside the app (Python from the APK);
#   7. a broken core makes the app restart itself (a new process comes up);
#   8. the media engine plays an audio file and a video file to their end through the
#      real sound output, and refuses a file outside the media folder.
# The app's SelfTest (debug builds) logs what its JobsStore receives; check_selftest.py
# checks it.
#
#   scripts/selftest_android.sh <debug apk>
set -euo pipefail

apk=$1
package=io.github.drybearr.elsewhere
check=$(dirname "$0")/check_selftest.py

launch() {
  adb logcat -c
  if [ -n "${1:-}" ]; then
    adb shell am start -W -n "$package/.MainActivity" --es selftest "$1" > /dev/null
  else
    adb shell am start -W -n "$package/.MainActivity" > /dev/null
  fi
}

stop() {
  adb shell am force-stop "$package"
}

# expect <step> <check...>: waits up to 3 minutes for the check to hold.
expect() {
  local step=$1 status
  shift
  for _ in $(seq 180); do
    adb logcat -d -s ElsewhereJobs:I > "$step.log"
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

adb install -r "$apk" > /dev/null
launch demo
expect 1-demo running 2
expect 1-demo revisions
stop
launch
expect 2-restart interrupted
stop
launch resume
expect 3-resume final done 19 1
expect 3-resume revisions
stop
launch long-demo
expect 4-demo running 1
stop
launch discard
expect 5-discard final cancelled
stop
launch ytdlp
expect 6-ytdlp ytdlp
stop
launch broken
expect 7-broken restarted
stop
launch player
expect 8-player player song.m4a clip.webm
stop
adb shell run-as "$package" files/bin/ffmpeg -hide_banner -version | head -1
echo "The job path works in the Android app."
