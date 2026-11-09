#!/usr/bin/env bash
# Builds libmpv for Android: the media engine's library (src/player).
#
#   scripts/android_mpv.sh [aarch64|x86_64]     (needs ANDROID_NDK_ROOT, a Linux host)
#
# The result is one libmpv.so with everything inside (FFmpeg, libass, libplacebo, ...),
# put in target/android/libs/<abi>/ for the APK. src/player loads it when the first
# player is made. Builds are kept in target/mpv/<arch> and reused while nothing here or
# in mpv_sources.sh changes.
#
# Made for local files only (see mpv_sources.sh): FFmpeg has no network code and reads
# files only; there is no Lua and no JavaScript. LGPL only. Sound goes out through AAudio
# or OpenSL ES.
set -euo pipefail

API=26
here=$(cd "$(dirname "$0")" && pwd)
source "$here/mpv_sources.sh"

arch=${1:-aarch64}
case $arch in
  aarch64) abi=arm64-v8a; cpu=armv8; asm=true ;;
  x86_64) abi=x86_64; cpu=x86_64; asm=false ;;  # the emulator; x86 assembly needs nasm
  *) echo "usage: $0 [aarch64|x86_64]" >&2; exit 1 ;;
esac
: "${ANDROID_NDK_ROOT:?set ANDROID_NDK_ROOT to the Android NDK}"

root=$(cd "$here/.." && pwd)
work=$root/target/mpv
out=$work/$arch
stamp="$(cat "$0" "$here/mpv_sources.sh" | sha256sum | cut -c1-16) $(basename "$ANDROID_NDK_ROOT")"
libs=$root/target/android/libs/$abi

if [ "$(cat "$out/stamp" 2>/dev/null)" = "$stamp" ] && [ -f "$out/libmpv.so" ]; then
  mkdir -p "$libs" && cp "$out/libmpv.so" "$libs/"
  echo "libmpv for $abi (kept): $(du -h "$libs/libmpv.so" | cut -f1)"
  exit 0
fi

toolchain=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin
prefix=$out/prefix
build=$out/build
sources=$work/sources
rm -rf "$out"
mkdir -p "$prefix/lib/pkgconfig" "$build" "$sources"
tools
write_pkg_config

triple=$arch-linux-android
cat > "$out/cross.txt" <<CROSS
[binaries]
c = '$toolchain/$triple$API-clang'
cpp = '$toolchain/$triple$API-clang++'
ar = '$toolchain/llvm-ar'
nm = '$toolchain/llvm-nm'
ranlib = '$toolchain/llvm-ranlib'
strip = '$toolchain/llvm-strip'
pkg-config = '$out/pkg-config'

[built-in options]
c_args = ['-fPIC']
cpp_args = ['-fPIC']
c_link_args = ['-Wl,-z,max-page-size=16384']
cpp_link_args = ['-Wl,-z,max-page-size=16384']
default_library = 'static'
buildtype = 'release'
prefix = '$prefix'
libdir = 'lib'
wrap_mode = 'nodownload'

[host_machine]
system = 'android'
cpu_family = '$arch'
cpu = '$cpu'
endian = 'little'
CROSS

fetch_all

meson_build dav1d -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_asm=$asm

ffmpeg_options=(
  --prefix="$prefix" --target-os=android --arch="$arch" --enable-cross-compile
  --cc="$toolchain/$triple$API-clang" --cxx="$toolchain/$triple$API-clang++"
  --ar="$toolchain/llvm-ar" --nm="$toolchain/llvm-nm" --ranlib="$toolchain/llvm-ranlib"
  --strip="$toolchain/llvm-strip" --pkg-config="$out/pkg-config"
  --enable-static --disable-shared --enable-pic "${FFMPEG_COMPONENTS[@]}"
  --enable-jni --enable-mediacodec
  --enable-decoder=h264_mediacodec,hevc_mediacodec,vp8_mediacodec,vp9_mediacodec,av1_mediacodec,mpeg4_mediacodec
)
[ "$asm" = true ] || ffmpeg_options+=(--disable-x86asm)
echo "Building ffmpeg"
(cd "$build/ffmpeg" && ./configure "${ffmpeg_options[@]}" > "$out/ffmpeg.log" 2>&1 &&
  make -j"$(nproc)" install >> "$out/ffmpeg.log" 2>&1) || { tail -40 "$out/ffmpeg.log"; exit 1; }

meson_build freetype -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=disabled -Dzlib=disabled -Dmmap=enabled
meson_build fribidi -Ddocs=false -Dbin=false -Dtests=false
meson_build harfbuzz -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dicu=disabled \
  -Dchafa=disabled -Dtests=disabled -Ddocs=disabled -Dutilities=disabled -Dintrospection=disabled
add_cxx_runtime "-lc++_static -lc++abi"
meson_build libass -Dfontconfig=disabled -Ddirectwrite=disabled -Dcoretext=disabled \
  -Drequire-system-font-provider=false -Dasm=disabled -Dlibunibreak=disabled -Dtest=disabled \
  -Dcompare=disabled -Dprofile=disabled -Dfuzz=disabled -Dcheckasm=disabled
meson_build libplacebo -Dvulkan=disabled -Dopengl=enabled -Dgl-proc-addr=enabled -Dglslang=disabled \
  -Dshaderc=disabled -Dlcms=disabled -Ddovi=disabled -Dlibdovi=disabled -Dd3d11=disabled \
  -Ddemos=false -Dtests=false -Dbench=false -Dfuzz=false -Dxxhash=disabled -Dunwind=disabled
# Only mpv_* is exported, and FFmpeg's av_jni_set_java_vm: the core hands it the Java VM,
# which mpv's video output needs to reach the app's Surface (src/player/android.rs). The
# libraries inside stay hidden.
printf '{\n  global: mpv_*; av_jni_set_java_vm;\n  local: *;\n};\n' > "$out/exports.map"
meson_build mpv -Ddefault_library=shared "${MPV_COMMON[@]}" \
  -Dgl=enabled -Degl-android=enabled -Dandroid-media-ndk=enabled -Daaudio=enabled -Dopensles=enabled \
  -Daudiotrack=enabled "-Dc_link_args=['-Wl,-z,max-page-size=16384', '-Wl,--version-script=$out/exports.map', '-Wl,--undefined-version']"
grep -q "enabled features:.* gpl " "$out/mpv.log" && { echo "mpv was configured as GPL" >&2; exit 1; }

"$toolchain/llvm-strip" --strip-unneeded -o "$out/libmpv.so" "$prefix/lib/libmpv.so"
symbols=$("$toolchain/llvm-readelf" --dyn-syms -W "$out/libmpv.so" | awk '$5 == "GLOBAL" && $7 != "UND" {print $8}' | sed 's/@.*//')
others=$(echo "$symbols" | grep -v '^mpv_' | grep -v '^av_jni_set_java_vm$' || true)
if [ -n "$others" ]; then
  echo "libmpv.so exports more than mpv_* and av_jni_set_java_vm: $(echo "$others" | tr '\n' ' ' | cut -c1-200)" >&2
  exit 1
fi
if ! grep -qx 'av_jni_set_java_vm' <<< "$symbols"; then
  echo "libmpv.so does not export av_jni_set_java_vm (needed for video)" >&2
  exit 1
fi
needed=$("$toolchain/llvm-readelf" -d "$out/libmpv.so" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | sort | tr '\n' ' ')
for lib in $needed; do
  case $lib in
    libc.so|libm.so|libdl.so|liblog.so|libandroid.so|libz.so|libEGL.so|libGLESv2.so|libGLESv3.so|libOpenSLES.so|libaaudio.so|libmediandk.so|libnativewindow.so) ;;
    *) echo "libmpv.so needs $lib, which Android does not have" >&2; exit 1 ;;
  esac
done
echo "$stamp" > "$out/stamp"
mkdir -p "$libs"
cp "$out/libmpv.so" "$libs/"
echo "libmpv $MPV (FFmpeg $FFMPEG) for $abi: $(du -h "$libs/libmpv.so" | cut -f1), needs $needed"
