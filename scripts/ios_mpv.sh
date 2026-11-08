#!/usr/bin/env bash
# Builds libmpv for iOS: the media engine's library (src/player), as
# target/ios/mpv.xcframework, with mpv.framework for iPhones and for the simulator.
#
#   scripts/ios_mpv.sh [device|simulator|all]     (macOS with Xcode and pkg-config)
#
# Run it after `scripts/mobile_python.py ios`, which empties target/ios. The app embeds
# the framework (ios/project.yml); src/player loads @rpath/mpv.framework/mpv when the first
# player is made. One dynamic library with everything inside (FFmpeg, libass, libplacebo,
# ...); only mpv_* is exported. Builds are kept in target/mpv-ios and reused while nothing
# here or in mpv_sources.sh changes.
#
# The same sources and limits as Android (mpv_sources.sh): local files only, no network
# code, no scripting, LGPL. Sound goes out through AudioUnit; FFmpeg can decode video on
# the GPU with VideoToolbox.
set -euo pipefail

MIN_IOS=16.0
here=$(cd "$(dirname "$0")" && pwd)
source "$here/mpv_sources.sh"
root=$(cd "$here/.." && pwd)
work=$root/target/mpv-ios
stamp="$(cat "$0" "$here/mpv_sources.sh" | shasum -a 256 | cut -c1-16) $(xcodebuild -version 2> /dev/null | tr "\n" " ")"
jobs=$(sysctl -n hw.ncpu)

# slice <iphoneos|iphonesimulator>: builds mpv.framework for that SDK into $work/<sdk>.
slice() {
  local sdk=$1 subsystem triple sysroot cc cxx
  out=$work/$sdk
  if [ "$(cat "$out/stamp" 2> /dev/null)" = "$stamp" ] && [ -f "$out/mpv.framework/mpv" ]; then
    echo "libmpv for $sdk (kept)"
    return
  fi
  case $sdk in
    iphoneos) subsystem=ios; triple=arm64-apple-ios$MIN_IOS ;;
    iphonesimulator) subsystem=ios-simulator; triple=arm64-apple-ios$MIN_IOS-simulator ;;
  esac
  sysroot=$(xcrun --sdk "$sdk" --show-sdk-path)
  cc=$(xcrun --sdk "$sdk" -f clang)
  cxx=$(xcrun --sdk "$sdk" -f clang++)
  prefix=$out/prefix
  build=$out/build
  sources=$work/sources
  rm -rf "$out"
  mkdir -p "$prefix/lib/pkgconfig" "$build" "$sources"
  write_pkg_config

  local flags="'-target', '$triple', '-isysroot', '$sysroot'"
  cat > "$out/cross.txt" << CROSS
[binaries]
c = '$cc'
cpp = '$cxx'
objc = '$cc'
objcpp = '$cxx'
ar = '$(xcrun --sdk "$sdk" -f ar)'
nm = '$(xcrun --sdk "$sdk" -f nm)'
ranlib = '$(xcrun --sdk "$sdk" -f ranlib)'
strip = '$(xcrun --sdk "$sdk" -f strip)'
pkg-config = '$out/pkg-config'

[built-in options]
c_args = [$flags, '-fPIC']
cpp_args = [$flags, '-fPIC']
objc_args = [$flags, '-fPIC']
objcpp_args = [$flags, '-fPIC']
c_link_args = [$flags]
cpp_link_args = [$flags]
objc_link_args = [$flags]
objcpp_link_args = [$flags]
default_library = 'static'
buildtype = 'release'
prefix = '$prefix'
libdir = 'lib'
wrap_mode = 'nodownload'

[properties]
needs_exe_wrapper = true

[host_machine]
system = 'darwin'
subsystem = '$subsystem'
kernel = 'xnu'
cpu_family = 'aarch64'
cpu = 'arm64'
endian = 'little'
CROSS

  fetch_all
  meson_build dav1d -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_asm=true

  echo "Building ffmpeg"
  (cd "$build/ffmpeg" && ./configure --prefix="$prefix" --enable-cross-compile --target-os=darwin \
    --arch=aarch64 --cc="$cc" --cxx="$cxx" --sysroot="$sysroot" \
    --extra-cflags="-target $triple" --extra-ldflags="-target $triple" \
    --ar="$(xcrun --sdk "$sdk" -f ar)" --nm="$(xcrun --sdk "$sdk" -f nm)" \
    --ranlib="$(xcrun --sdk "$sdk" -f ranlib)" --strip="$(xcrun --sdk "$sdk" -f strip)" \
    --pkg-config="$out/pkg-config" --enable-static --disable-shared --enable-pic \
    "${FFMPEG_COMPONENTS[@]}" --enable-videotoolbox \
    --enable-hwaccel=h264_videotoolbox,hevc_videotoolbox,vp9_videotoolbox,av1_videotoolbox,mpeg4_videotoolbox \
    > "$out/ffmpeg.log" 2>&1 && make -j"$jobs" install >> "$out/ffmpeg.log" 2>&1) ||
    { tail -40 "$out/ffmpeg.log"; exit 1; }

  meson_build freetype -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=disabled -Dzlib=disabled -Dmmap=enabled
  meson_build fribidi -Ddocs=false -Dbin=false -Dtests=false
  meson_build harfbuzz -Dfreetype=enabled -Dcoretext=disabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled \
    -Dicu=disabled -Dchafa=disabled -Dtests=disabled -Ddocs=disabled -Dutilities=disabled -Dintrospection=disabled
  add_cxx_runtime "-lc++"
  meson_build libass -Dfontconfig=disabled -Ddirectwrite=disabled -Dcoretext=enabled \
    -Drequire-system-font-provider=false -Dasm=disabled -Dlibunibreak=disabled -Dtest=disabled \
    -Dcompare=disabled -Dprofile=disabled -Dfuzz=disabled -Dcheckasm=disabled
  meson_build libplacebo -Dvulkan=disabled -Dopengl=disabled -Dglslang=disabled -Dshaderc=disabled \
    -Dlcms=disabled -Ddovi=disabled -Dlibdovi=disabled -Dd3d11=disabled -Ddemos=false -Dtests=false \
    -Dbench=false -Dfuzz=false -Dxxhash=disabled -Dunwind=disabled
  # mpv asks for the macOS SDK on any Apple system; these make it use the iOS one.
  export MACOS_SDK=$sysroot MACOS_SDK_VERSION
  MACOS_SDK_VERSION=$(xcrun --sdk "$sdk" --show-sdk-version)
  # Only mpv_* is exported: the libraries inside stay hidden.
  meson_build mpv -Ddefault_library=shared "${MPV_COMMON[@]}" -Daudiounit=enabled -Dgl=disabled \
    "-Dc_link_args=[$flags, '-Wl,-exported_symbol,_mpv_*', '-Wl,-dead_strip']"
  unset MACOS_SDK MACOS_SDK_VERSION
  grep -q "enabled features:.* gpl " "$out/mpv.log" && { echo "mpv was configured as GPL" >&2; exit 1; }

  local framework=$out/mpv.framework library
  library=$(find "$prefix/lib" -name 'libmpv.*.dylib' -type f -print -quit)
  mkdir -p "$framework"
  cp "$library" "$framework/mpv"
  install_name_tool -id @rpath/mpv.framework/mpv "$framework/mpv"
  strip -x "$framework/mpv"
  local platform=iPhoneOS
  [ "$sdk" = iphonesimulator ] && platform=iPhoneSimulator
  cat > "$framework/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>mpv</string>
  <key>CFBundleIdentifier</key><string>io.github.drybearr.elsewhere.mpv</string>
  <key>CFBundleName</key><string>mpv</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>$MPV</string>
  <key>CFBundleVersion</key><string>$MPV</string>
  <key>CFBundleSupportedPlatforms</key><array><string>$platform</string></array>
  <key>MinimumOSVersion</key><string>$MIN_IOS</string>
</dict>
</plist>
PLIST

  local exported
  exported=$(nm -gU "$framework/mpv" | awk '{print $3}')
  if echo "$exported" | grep -qv '^_mpv_'; then
    echo "mpv.framework exports more than mpv_*: $(echo "$exported" | grep -v '^_mpv_' | head -3 | tr '\n' ' ')" >&2
    exit 1
  fi
  # Only the system's own libraries and frameworks may be needed.
  if otool -L "$framework/mpv" | tail -n +2 | grep -v -e '@rpath/mpv.framework/mpv' -e ' /usr/lib/' -e ' /System/Library/'; then
    echo "mpv.framework needs a library iOS does not have (above)" >&2
    exit 1
  fi
  echo "$stamp" > "$out/stamp"
  echo "libmpv $MPV (FFmpeg $FFMPEG) for $sdk: $(du -h "$framework/mpv" | cut -f1)"
}

case ${1:-all} in
  device) sdks="iphoneos" ;;
  simulator) sdks="iphonesimulator" ;;
  all) sdks="iphoneos iphonesimulator" ;;
  *) echo "usage: $0 [device|simulator|all]" >&2; exit 1 ;;
esac
mkdir -p "$work"
tools
args=()
for sdk in $sdks; do
  slice "$sdk"
  args+=(-framework "$work/$sdk/mpv.framework")
done
rm -rf "$root/target/ios/mpv.xcframework"
mkdir -p "$root/target/ios"
xcodebuild -create-xcframework "${args[@]}" -output "$root/target/ios/mpv.xcframework" > /dev/null
echo "mpv.xcframework: $(du -sh "$root/target/ios/mpv.xcframework" | cut -f1)"
