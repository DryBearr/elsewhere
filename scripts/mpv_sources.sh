# The sources of libmpv, the same for Android (android_mpv.sh) and iOS (ios_mpv.sh), and
# their checksums. Change a version here and both builds follow.
FFMPEG=9.0.2 FFMPEG_SHA=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
DAV1D=1.5.4 DAV1D_SHA=686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5
FREETYPE=2.14.3 FREETYPE_SHA=36bc4f1cc413335368ee656c42afca65c5a3987e8768cc28cf11ba775e785a5f
FRIBIDI=1.0.17 FRIBIDI_SHA=6949dcde27d41cebad1fd741fcafc36d55a1020d2d872d4a6eb3914caabbada2
HARFBUZZ=14.6.0 HARFBUZZ_SHA=d07a007327277708a2a73ae437887cdbaf282937f6d03ca5467723e9099af586
LIBASS=0.17.5 LIBASS_SHA=2dca25c0e0c837ddf00b52011b3f82cac1e4ddd3ad018227806b0c2288864acc
LIBPLACEBO=v7.360.1 LIBPLACEBO_COMMIT=cee9b076f2c63104ccfd497fa79c39a867293ec4
MPV=0.41.0 MPV_SHA=ee21092a5ee427353392360929dc64645c54479aefdb5babc5cfbb5fad626209
TOOLS="meson==1.12.1 ninja==1.13.2 jinja2==3.1.6"

# What FFmpeg reads and decodes: local files of the kinds the library holds. No network.
FFMPEG_COMPONENTS=(
  --disable-everything --disable-autodetect --disable-network --disable-avdevice
  --disable-programs --disable-doc --disable-debug --enable-zlib --enable-libdav1d
  --enable-protocol=file
  --enable-demuxer=mov,matroska,webm_dash_manifest,mp3,aac,ac3,eac3,dts,truehd,flac,ogg,wav,w64,aiff,wv,ape,asf,avi,mpegts,mpegps,mpegvideo,h264,hevc,m4v,ass,srt,webvtt,image2,mjpeg
  --enable-decoder=h264,hevc,vp8,vp9,av1,libdav1d,mpeg4,mpeg2video,mpeg1video,msmpeg4v3,mjpeg,png,bmp,gif,webp,theora,vc1,wmv3
  --enable-decoder=aac,aac_fixed,aac_latm,mp1,mp2,mp3,mp3float,opus,vorbis,flac,alac,ac3,eac3,dca,truehd,mlp,wavpack,ape,wmav1,wmav2,wmapro,als
  --enable-decoder=pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_f32le,pcm_f64le,pcm_u8,pcm_alaw,pcm_mulaw,pcm_bluray,pcm_dvd
  --enable-decoder=ass,ssa,srt,subrip,webvtt,mov_text,text,dvdsub,dvbsub,pgssub
  --enable-parser=aac,aac_latm,ac3,av1,dca,flac,h264,hevc,mjpeg,mlp,mpeg4video,mpegaudio,mpegvideo,opus,png,vc1,vorbis,vp8,vp9,webp
  --enable-bsfs
  --enable-filter=buffer,buffersink,abuffer,abuffersink,aformat,format,anull,null,aresample,scale,volume,hflip,vflip,transpose,rotate,crop,pad
)

# mpv's options both builds share: LGPL, libmpv only, nothing scripted.
MPV_COMMON=(
  -Dlibmpv=true -Dcplayer=false -Dgpl=false -Dbuild-date=false -Dtests=false -Dfuzzers=false
  -Dauto_features=disabled -Dlua=disabled -Djavascript=disabled -Dzlib=enabled
)

# sum_ok <sha256> <file>
sum_ok() {
  local found
  if command -v sha256sum > /dev/null; then found=$(sha256sum "$2" | cut -d' ' -f1); else found=$(shasum -a 256 "$2" | cut -d' ' -f1); fi
  [ "$found" = "$1" ]
}

# fetch <url> <sha256> <name>: downloads once into $sources, checks, unpacks into $build/<name>.
fetch() {
  local url=$1 sum=$2 name=$3 file=$sources/${1##*/}
  if [ ! -f "$file" ] || ! sum_ok "$sum" "$file"; then
    echo "Downloading $name"
    # Servers reset connections now and then: try again on any error.
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 --connect-timeout 30 "$url" -o "$file"
    sum_ok "$sum" "$file" || { echo "$file: wrong checksum" >&2; exit 1; }
  fi
  rm -rf "${build:?}/$name"
  mkdir -p "$build/$name"
  tar xf "$file" -C "$build/$name" --strip-components=1
}

fetch_all() {
  fetch "https://downloads.videolan.org/pub/videolan/dav1d/$DAV1D/dav1d-$DAV1D.tar.xz" $DAV1D_SHA dav1d
  fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG.tar.xz" $FFMPEG_SHA ffmpeg
  fetch "https://download.savannah.gnu.org/releases/freetype/freetype-$FREETYPE.tar.xz" $FREETYPE_SHA freetype
  fetch "https://github.com/fribidi/fribidi/releases/download/v$FRIBIDI/fribidi-$FRIBIDI.tar.xz" $FRIBIDI_SHA fribidi
  fetch "https://github.com/harfbuzz/harfbuzz/releases/download/$HARFBUZZ/harfbuzz-$HARFBUZZ.tar.xz" $HARFBUZZ_SHA harfbuzz
  fetch "https://github.com/libass/libass/releases/download/$LIBASS/libass-$LIBASS.tar.xz" $LIBASS_SHA libass
  fetch "https://github.com/mpv-player/mpv/archive/refs/tags/v$MPV.tar.gz" $MPV_SHA mpv
  echo "Downloading libplacebo"
  local try
  for try in 1 2 3 4; do
    rm -rf "$build/libplacebo"
    git clone --quiet --depth 1 --branch $LIBPLACEBO --recurse-submodules --shallow-submodules \
      https://code.videolan.org/videolan/libplacebo.git "$build/libplacebo" 2> /dev/null && break
    [ $try = 4 ] && { echo "could not download libplacebo" >&2; exit 1; }
    sleep 10
  done
  [ "$(git -C "$build/libplacebo" rev-parse HEAD)" = $LIBPLACEBO_COMMIT ] ||
    { echo "libplacebo $LIBPLACEBO is not commit $LIBPLACEBO_COMMIT" >&2; exit 1; }
}

# The meson build tools, in their own Python environment. A runner's Python can come
# without a working ensurepip, so the next Python is tried (macOS always has Apple's).
tools() {
  if [ ! -x "$work/venv/bin/meson" ]; then
    local python made=""
    for python in python3 /usr/bin/python3 python3.13 python3.12; do
      command -v "$python" > /dev/null || continue
      rm -rf "$work/venv"
      if "$python" -m venv "$work/venv" > /dev/null 2>&1 &&
        "$work/venv/bin/pip" install --quiet --disable-pip-version-check $TOOLS; then
        made=yes
        break
      fi
      echo "$python could not make the build tools' environment; trying the next one"
    done
    [ -n "$made" ] || { echo "no Python could make the build tools' environment" >&2; exit 1; }
  fi
  export PATH=$work/venv/bin:$PATH
}

# meson_build <name> <options...>: builds and installs into $prefix with $out/cross.txt.
meson_build() {
  local name=$1
  shift
  echo "Building $name"
  (cd "$build/$name" && meson setup _build --cross-file "$out/cross.txt" "$@" > "$out/$name.log" 2>&1 &&
    ninja -C _build install >> "$out/$name.log" 2>&1) || { tail -40 "$out/$name.log"; exit 1; }
}

# pkg-config that only sees what is built in $prefix, linking statically.
write_pkg_config() {
  cat > "$out/pkg-config" << SCRIPT
#!/bin/sh
export PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig PKG_CONFIG_PATH=
exec pkg-config --static "\$@"
SCRIPT
  chmod +x "$out/pkg-config"
}

# HarfBuzz is C++: whoever links it statically needs the C++ runtime too ($1).
add_cxx_runtime() {
  local pc=$prefix/lib/pkgconfig/harfbuzz.pc
  if grep -q '^Libs.private:' "$pc"; then
    sed -i.bak "s/^Libs.private: \(.*\)\$/Libs.private: \1 $1/" "$pc"
  else
    echo "Libs.private: $1" >> "$pc"
  fi
}
