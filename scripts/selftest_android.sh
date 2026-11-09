#!/usr/bin/env bash
# Checks the job path through the real Android app on an emulator (adb connected):
#   1. a demo job is killed halfway, 2. comes back Interrupted, 3. Resume ends it Done
#   with exact counts and the screen equal to the core; 4-5. another is discarded and
#   ends Cancelled; 6. yt-dlp starts lazily inside the app (Python from the APK);
#   7. a broken core makes the app restart itself (a new process comes up);
#   8. the media engine plays an audio file and a video file to their end through the
#      real sound output, and refuses a file outside the media folder;
#   9. the playlist player plays on behind the home screen and with the screen off, and
#      the system controls it: media keys (lock screen, Bluetooth, headphones), a call, a
#      navigation voice, another app playing, unplugged headphones.
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

# What the system is sent: a media key, or a self-test action (SelfTestReceiver).
key() {
  adb logcat -c
  adb shell input keyevent "$1"
}

act() {
  adb logcat -c
  adb shell am broadcast -n "$package/.SelfTestReceiver" --es action "$1" > /dev/null
}

# expect_system <step> session|service <wanted>: waits up to 30 s for what the system shows
# (the app's media session, its playback service) to hold.
expect_system() {
  local step=$1 status dump
  shift
  case $1 in
    session) dump=media_session ;;
    focus) dump=audio ;;
    *) dump="activity services $package" ;;
  esac
  for _ in $(seq 30); do
    # shellcheck disable=SC2086 # the dumpsys arguments are words
    adb shell dumpsys $dump > "$step.dump"
    status=0
    python3 "$check" "$step.dump" "$@" || status=$?
    if [ "$status" -eq 0 ]; then
      echo "ok: $step ($*)"
      return 0
    fi
    [ "$status" -ge 2 ] && break
    sleep 1
  done
  echo "FAILED: $step ($*)"
  grep -A 12 "$package" "$step.dump" || cat "$step.dump"
  adb logcat -d -s Elsewhere:I ElsewhereJobs:I | tail -40
  exit 1
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
  adb logcat -d -s Elsewhere:I AndroidRuntime:E | tail -40
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

# 9. Background playback. The test songs are 6 s and 2 s long, on repeat.
launch background
expect 9-play queue status=playing wants=true title=long
expect_system 9-play session 3
expect_system 9-play service foreground
expect_system 9-play focus held
key KEYCODE_HOME
expect 9-home queue-advancing 3000
key KEYCODE_SLEEP
expect 9-screen-off queue-advancing 3000
adb shell input keyevent KEYCODE_WAKEUP
adb shell wm dismiss-keyguard || true

# Media keys go to the session while the app is in the background.
key KEYCODE_MEDIA_PAUSE
expect 9-key-pause queue status=paused wants=false
expect_system 9-key-pause session 2
expect_system 9-key-pause service background
# Paused in the background for a while: a key still brings it back (and to the foreground).
sleep 15
key KEYCODE_MEDIA_PLAY
expect 9-key-play queue status=playing wants=true
expect_system 9-key-play service foreground
key KEYCODE_MEDIA_PAUSE
expect 9-key-pause-2 queue status=paused wants=false
act report
adb shell input keyevent KEYCODE_MEDIA_NEXT
expect 9-key-next queue-moved
expect 9-key-next queue wants=false
act report
adb shell input keyevent KEYCODE_MEDIA_PREVIOUS
expect 9-key-previous queue-moved
key KEYCODE_MEDIA_PLAY_PAUSE
expect 9-key-play-pause queue status=playing wants=true

# A call: paused, and playing again after it, in the foreground all along.
act focus-transient
expect 9-call queue status=paused wants=false keeps=true
expect_system 9-call service foreground
act focus-return
expect 9-call-over queue status=playing wants=true
# Paused by the person during a call: it stays paused after the call.
act focus-transient
expect 9-call-pause queue wants=false keeps=true
key KEYCODE_MEDIA_PAUSE
expect 9-call-pause queue keeps=false
act focus-return
sleep 3
expect 9-call-pause-over queue-absent wants=true
expect_system 9-call-pause-over service background
key KEYCODE_MEDIA_PLAY
expect 9-call-pause-play queue status=playing wants=true

# A navigation voice: quieter while it speaks, playing all along.
act focus-duck
expect 9-voice queue volume=20.0 wants=true
act focus-return
expect 9-voice-over queue volume=100.0 status=playing wants=true

# Another app plays: paused for good.
act focus-permanent
expect 9-other-app queue status=paused wants=false
act focus-return
sleep 3
expect 9-other-app-over queue-absent wants=true
key KEYCODE_MEDIA_PLAY
expect 9-other-app-play queue status=playing wants=true

# Headphones unplugged.
act noisy
expect 9-noisy queue status=paused wants=false

# The end of the queue: the focus goes back, the notification stays, and Play starts over.
act repeat-off
key KEYCODE_MEDIA_PLAY
expect 9-end queue finished=true wants=false
expect_system 9-end session 1
expect_system 9-end service background
expect_system 9-end focus none
key KEYCODE_MEDIA_PLAY
expect 9-end-play queue status=playing wants=true current=0
expect_system 9-end-play focus held

# Stop ends it all: the queue, the session, the notification and the service.
key KEYCODE_MEDIA_STOP
expect 9-stop queue entries=0
expect_system 9-stop session none
expect_system 9-stop service none
stop
adb shell run-as "$package" files/bin/ffmpeg -hide_banner -version | head -1
echo "The job path works in the Android app."
