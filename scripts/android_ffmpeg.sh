#!/usr/bin/env bash
# Builds ffmpeg and ffprobe for Android, for yt-dlp to merge and fix up downloads.
#
#   scripts/android_ffmpeg.sh [aarch64|x86_64]     (needs ANDROID_NDK_ROOT, a Linux host)
#
# Run it after `scripts/mobile_python.py android`, which empties target/android.
#
# Android lets apps run only programs that ship as native libraries, so the results are
# named libffmpeg.so and libffprobe.so and go to target/android/libs/<abi>/, from where
# cargo-apk puts them in the APK. src/platform/android.rs links them as ffmpeg and
# ffprobe in a folder on PATH, where yt-dlp finds them.
#
# Only what yt-dlp needs without re-encoding is built: merging video and audio, fixing
# up m4a and HLS downloads, and writing metadata and chapters. Converting audio
# (-x --audio-format mp3 and similar) and embedding thumbnails need parts left out.
# The build is LGPL-only. Results are kept in target/ffmpeg/<arch> and reused.
set -euo pipefail

VERSION=9.0.2
SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
API=26

arch=${1:-aarch64}
case $arch in
  aarch64) abi=arm64-v8a; cpu=armv8-a; extra=() ;;
  x86_64) abi=x86_64; cpu=x86-64; extra=(--disable-x86asm) ;;
  *) echo "usage: $0 [aarch64|x86_64]" >&2; exit 1 ;;
esac
: "${ANDROID_NDK_ROOT:?set ANDROID_NDK_ROOT to the Android NDK}"

root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/target/ffmpeg
out=$work/$arch
toolchain=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin

options=(
  --target-os=android --arch="$arch" --cpu="$cpu" --enable-cross-compile
  --cc="$toolchain/$arch-linux-android$API-clang"
  --cxx="$toolchain/$arch-linux-android$API-clang++"
  --ar="$toolchain/llvm-ar" --nm="$toolchain/llvm-nm"
  --ranlib="$toolchain/llvm-ranlib" --strip="$toolchain/llvm-strip"
  --enable-pic --extra-ldflags=-Wl,-z,max-page-size=16384
  --disable-everything --disable-autodetect --disable-doc --disable-debug
  --disable-ffplay --disable-network --disable-avdevice --enable-zlib
  --enable-protocol=file,pipe
  --enable-demuxer=mov,matroska,mpegts,aac,mp3,ogg,flac,wav,ffmetadata,webvtt
  --enable-muxer=mp4,mov,ipod,matroska,webm,mp3,adts,ogg,opus,flac,wav,mpegts,ffmetadata
  --enable-parser=aac,aac_latm,h264,hevc,vp8,vp9,av1,opus,vorbis,mpegaudio,flac
  --enable-bsf=aac_adtstoasc,h264_mp4toannexb,hevc_mp4toannexb,vp9_superframe,extract_extradata
  "${extra[@]}"
)
stamp="$VERSION $(printf '%s\n' "${options[@]}" | sha256sum | cut -c1-16)"

if [ "$(cat "$out/stamp" 2>/dev/null)" != "$stamp" ]; then
  mkdir -p "$work"
  tarball=$work/ffmpeg-$VERSION.tar.xz
  if [ ! -f "$tarball" ] || ! echo "$SHA256  $tarball" | sha256sum -c --quiet; then
    echo "Downloading ffmpeg $VERSION"
    curl -fsSL "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" -o "$tarball"
    echo "$SHA256  $tarball" | sha256sum -c --quiet
  fi
  rm -rf "$work/src" "$work/build-$arch" "$out"
  mkdir -p "$work/src" "$work/build-$arch" "$out"
  tar xJf "$tarball" -C "$work/src" --strip-components=1

  cd "$work/build-$arch"
  "$work/src/configure" "${options[@]}" > configure.log || { tail -40 configure.log; exit 1; }
  make -j"$(nproc)" ffmpeg ffprobe > make.log || { tail -40 make.log; exit 1; }
  cp ffmpeg ffprobe "$out/"
  echo "$stamp" > "$out/stamp"
fi

libs=$root/target/android/libs/$abi
mkdir -p "$libs"
cp "$out/ffmpeg" "$libs/libffmpeg.so"
cp "$out/ffprobe" "$libs/libffprobe.so"
echo "ffmpeg $VERSION for $abi: $(du -h "$libs/libffmpeg.so" | cut -f1) + $(du -h "$libs/libffprobe.so" | cut -f1)"
