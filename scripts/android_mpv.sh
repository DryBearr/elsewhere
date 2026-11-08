#!/usr/bin/env bash
# Builds libmpv for Android: the media engine's library (src/player).
#
#   scripts/android_mpv.sh [aarch64|x86_64]     (needs ANDROID_NDK_ROOT, a Linux host)
#
# The result is one libmpv.so with everything inside (FFmpeg, libass, libplacebo, ...),
# put in target/android/libs/<abi>/ for the APK. src/player loads it when the first
# player is made. Builds are kept in target/mpv/<arch> and reused while nothing here
# changes.
#
# Made for local files only: FFmpeg has no network code (no http, no TLS) and reads
# files only; there is no Lua, no JavaScript, no libarchive. Decoders cover what the
# library holds: H.264, HEVC, VP8, VP9, AV1 (dav1d), MPEG-4, AAC, MP3, Opus, Vorbis,
# FLAC, ALAC, AC-3, E-AC-3, DTS, PCM, and text and picture subtitles. LGPL only.
set -euo pipefail

API=26
FFMPEG=9.0.2 FFMPEG_SHA=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
DAV1D=1.5.4 DAV1D_SHA=686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5
FREETYPE=2.14.3 FREETYPE_SHA=36bc4f1cc413335368ee656c42afca65c5a3987e8768cc28cf11ba775e785a5f
FRIBIDI=1.0.17 FRIBIDI_SHA=6949dcde27d41cebad1fd741fcafc36d55a1020d2d872d4a6eb3914caabbada2
HARFBUZZ=14.6.0 HARFBUZZ_SHA=d07a007327277708a2a73ae437887cdbaf282937f6d03ca5467723e9099af586
LIBASS=0.17.5 LIBASS_SHA=2dca25c0e0c837ddf00b52011b3f82cac1e4ddd3ad018227806b0c2288864acc
LIBPLACEBO=v7.360.1 LIBPLACEBO_COMMIT=cee9b076f2c63104ccfd497fa79c39a867293ec4
MPV=0.41.0 MPV_SHA=ee21092a5ee427353392360929dc64645c54479aefdb5babc5cfbb5fad626209
TOOLS="meson==1.12.1 ninja==1.13.2 jinja2==3.1.6"

arch=${1:-aarch64}
case $arch in
  aarch64) abi=arm64-v8a; cpu=armv8; asm=true ;;
  x86_64) abi=x86_64; cpu=x86_64; asm=false ;;  # the emulator; x86 assembly needs nasm
  *) echo "usage: $0 [aarch64|x86_64]" >&2; exit 1 ;;
esac
: "${ANDROID_NDK_ROOT:?set ANDROID_NDK_ROOT to the Android NDK}"

root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/target/mpv
out=$work/$arch
stamp="$(sha256sum "$0" | cut -c1-16) $(basename "$ANDROID_NDK_ROOT")"
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

# Build tools in their own Python environment, the same here and in CI.
if [ ! -x "$work/venv/bin/meson" ]; then
  python3 -m venv "$work/venv"
  "$work/venv/bin/pip" install --quiet $TOOLS
fi
export PATH=$work/venv/bin:$PATH

# fetch <url> <sha256> <name>: downloads once, checks, unpacks into $build/<name>.
fetch() {
  local url=$1 sum=$2 name=$3 file=$sources/${1##*/}
  if [ ! -f "$file" ] || ! echo "$sum  $file" | sha256sum -c --quiet 2>/dev/null; then
    echo "Downloading $name"
    curl -fsSL --retry 3 "$url" -o "$file"
    echo "$sum  $file" | sha256sum -c --quiet
  fi
  mkdir -p "$build/$name"
  tar xf "$file" -C "$build/$name" --strip-components=1
}

# pkg-config that only sees what is built here, with static linking.
cat > "$out/pkg-config" <<EOF
#!/bin/sh
export PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig PKG_CONFIG_PATH=
exec pkg-config --static "\$@"
EOF
chmod +x "$out/pkg-config"

triple=$arch-linux-android
cat > "$out/cross.txt" <<EOF
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
EOF

# meson_build <name> <options...>
meson_build() {
  local name=$1
  shift
  echo "Building $name"
  (cd "$build/$name" && meson setup _build --cross-file "$out/cross.txt" "$@" > "$out/$name.log" 2>&1 &&
    ninja -C _build install >> "$out/$name.log" 2>&1) || { tail -40 "$out/$name.log"; exit 1; }
}

fetch "https://downloads.videolan.org/pub/videolan/dav1d/$DAV1D/dav1d-$DAV1D.tar.xz" $DAV1D_SHA dav1d
meson_build dav1d -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_asm=$asm

fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG.tar.xz" $FFMPEG_SHA ffmpeg
ffmpeg_options=(
  --prefix="$prefix" --target-os=android --arch="$arch" --enable-cross-compile
  --cc="$toolchain/$triple$API-clang" --cxx="$toolchain/$triple$API-clang++"
  --ar="$toolchain/llvm-ar" --nm="$toolchain/llvm-nm" --ranlib="$toolchain/llvm-ranlib"
  --strip="$toolchain/llvm-strip" --pkg-config="$out/pkg-config"
  --enable-static --disable-shared --enable-pic --disable-programs --disable-doc
  --disable-debug --disable-everything --disable-autodetect --disable-network
  --disable-avdevice --enable-zlib --enable-libdav1d --enable-jni --enable-mediacodec
  --enable-protocol=file
  --enable-demuxer=mov,matroska,webm_dash_manifest,mp3,aac,ac3,eac3,dts,truehd,flac,ogg,wav,w64,aiff,wv,ape,asf,avi,mpegts,mpegps,mpegvideo,h264,hevc,m4v,ass,srt,webvtt,image2,mjpeg
  --enable-decoder=h264,hevc,vp8,vp9,av1,libdav1d,mpeg4,mpeg2video,mpeg1video,msmpeg4v3,mjpeg,png,bmp,gif,webp,theora,vc1,wmv3
  --enable-decoder=h264_mediacodec,hevc_mediacodec,vp8_mediacodec,vp9_mediacodec,av1_mediacodec,mpeg4_mediacodec
  --enable-decoder=aac,aac_fixed,aac_latm,mp1,mp2,mp3,mp3float,opus,vorbis,flac,alac,ac3,eac3,dca,truehd,mlp,wavpack,ape,wmav1,wmav2,wmapro,als
  --enable-decoder=pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_f32le,pcm_f64le,pcm_u8,pcm_alaw,pcm_mulaw,pcm_bluray,pcm_dvd
  --enable-decoder=ass,ssa,srt,subrip,webvtt,mov_text,text,dvdsub,dvbsub,pgssub
  --enable-parser=aac,aac_latm,ac3,av1,dca,flac,h264,hevc,mjpeg,mlp,mpeg4video,mpegaudio,mpegvideo,opus,png,vc1,vorbis,vp8,vp9,webp
  --enable-bsfs
  --enable-filter=buffer,buffersink,abuffer,abuffersink,aformat,format,anull,null,aresample,scale,volume,hflip,vflip,transpose,rotate,crop,pad
)
[ "$asm" = true ] || ffmpeg_options+=(--disable-x86asm)
echo "Building ffmpeg"
(cd "$build/ffmpeg" && ./configure "${ffmpeg_options[@]}" > "$out/ffmpeg.log" 2>&1 &&
  make -j"$(nproc)" install >> "$out/ffmpeg.log" 2>&1) || { tail -40 "$out/ffmpeg.log"; exit 1; }

fetch "https://download.savannah.gnu.org/releases/freetype/freetype-$FREETYPE.tar.xz" $FREETYPE_SHA freetype
meson_build freetype -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=disabled -Dzlib=disabled -Dmmap=enabled

fetch "https://github.com/fribidi/fribidi/releases/download/v$FRIBIDI/fribidi-$FRIBIDI.tar.xz" $FRIBIDI_SHA fribidi
meson_build fribidi -Ddocs=false -Dbin=false -Dtests=false

fetch "https://github.com/harfbuzz/harfbuzz/releases/download/$HARFBUZZ/harfbuzz-$HARFBUZZ.tar.xz" $HARFBUZZ_SHA harfbuzz
meson_build harfbuzz -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dicu=disabled \
  -Dchafa=disabled -Dtests=disabled -Ddocs=disabled -Dutilities=disabled -Dintrospection=disabled
# HarfBuzz is C++: whoever links it statically needs the C++ runtime too.
sed -i 's/^Libs.private: \(.*\)$/Libs.private: \1 -lc++_static -lc++abi/' "$prefix/lib/pkgconfig/harfbuzz.pc"
grep -q '^Libs.private:.*c++_static' "$prefix/lib/pkgconfig/harfbuzz.pc" ||
  echo 'Libs.private: -lc++_static -lc++abi' >> "$prefix/lib/pkgconfig/harfbuzz.pc"

fetch "https://github.com/libass/libass/releases/download/$LIBASS/libass-$LIBASS.tar.xz" $LIBASS_SHA libass
meson_build libass -Dfontconfig=disabled -Ddirectwrite=disabled -Dcoretext=disabled \
  -Drequire-system-font-provider=false -Dasm=disabled -Dlibunibreak=disabled -Dtest=disabled \
  -Dcompare=disabled -Dprofile=disabled -Dfuzz=disabled -Dcheckasm=disabled

echo "Downloading libplacebo"
git clone --quiet --depth 1 --branch $LIBPLACEBO --recurse-submodules --shallow-submodules \
  https://code.videolan.org/videolan/libplacebo.git "$build/libplacebo" 2> /dev/null
[ "$(git -C "$build/libplacebo" rev-parse HEAD)" = $LIBPLACEBO_COMMIT ] ||
  { echo "libplacebo $LIBPLACEBO is not commit $LIBPLACEBO_COMMIT" >&2; exit 1; }
meson_build libplacebo -Dvulkan=disabled -Dopengl=enabled -Dgl-proc-addr=enabled -Dglslang=disabled \
  -Dshaderc=disabled -Dlcms=disabled -Ddovi=disabled -Dlibdovi=disabled -Dd3d11=disabled \
  -Ddemos=false -Dtests=false -Dbench=false -Dfuzz=false -Dxxhash=disabled -Dunwind=disabled

fetch "https://github.com/mpv-player/mpv/archive/refs/tags/v$MPV.tar.gz" $MPV_SHA mpv
# LGPL (gpl=false). Only mpv_* is exported: the libraries inside stay hidden.
meson_build mpv -Ddefault_library=shared -Dlibmpv=true -Dcplayer=false -Dgpl=false -Dbuild-date=false \
  -Dtests=false -Dfuzzers=false -Dauto_features=disabled -Dlua=disabled -Djavascript=disabled \
  -Dgl=enabled -Degl-android=enabled -Dandroid-media-ndk=enabled -Daaudio=enabled -Dopensles=enabled \
  -Daudiotrack=enabled -Dzlib=enabled \
  "-Dc_link_args=['-Wl,-z,max-page-size=16384', '-Wl,--exclude-libs,ALL']"
grep -q "enabled features:.* gpl " "$out/mpv.log" && { echo "mpv was configured as GPL" >&2; exit 1; }

"$toolchain/llvm-strip" --strip-unneeded -o "$out/libmpv.so" "$prefix/lib/libmpv.so"
exported=$("$toolchain/llvm-readelf" --dyn-syms -W "$out/libmpv.so" | awk '$5 == "GLOBAL" && $7 != "UND" {print $8}')
if echo "$exported" | grep -qv '^mpv_'; then
  echo "libmpv.so exports more than mpv_*: $(echo "$exported" | grep -v '^mpv_' | head -3 | tr '\n' ' ')" >&2
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
