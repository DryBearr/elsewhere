#!/usr/bin/env bash
# Builds the Rust core for Android and writes its Kotlin bindings.
#
#   scripts/android_rust.sh [aarch64|x86_64] [release|debug]    (needs ANDROID_NDK_ROOT)
#
# Run it after `scripts/mobile_python.py android`, which empties target/android and puts
# the Python build PyO3 links against in target/android/prefix. Writes:
#   target/android/libs/<abi>/libelsewhere.so   the core, which the app loads
#   target/android/kotlin/                      the Kotlin bindings for src/api.rs
set -euo pipefail

arch=${1:-aarch64}
profile=${2:-release}
case $arch in
  aarch64) abi=arm64-v8a ;;
  x86_64) abi=x86_64 ;;
  *) echo "usage: $0 [aarch64|x86_64] [release|debug]" >&2; exit 1 ;;
esac
: "${ANDROID_NDK_ROOT:?set ANDROID_NDK_ROOT to the Android NDK}"

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=$arch-linux-android
toolchain=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin
var=${target//-/_}
export "CARGO_TARGET_${var^^}_LINKER=$toolchain/${target}26-clang"
export "CC_$var=$toolchain/${target}26-clang" "AR_$var=$toolchain/llvm-ar"
export PYO3_PYTHON=${PYO3_PYTHON:-python3}
export PYO3_CROSS_LIB_DIR=$root/target/android/prefix/lib

# (No empty arrays: macOS bash 3.2 treats them as unset under set -u.)
flags=(--profile dev)
[ "$profile" = release ] && flags=(--release)
cargo rustc --lib --features playlists --target "$target" "${flags[@]}" --crate-type cdylib
library=target/$target/$profile/libelsewhere.so
mkdir -p "target/android/libs/$abi"
cp "$library" "target/android/libs/$abi/"

rm -rf target/android/kotlin
cargo run --quiet --features bindgen --bin uniffi-bindgen -- \
  generate --library "$library" --language kotlin --no-format --out-dir target/android/kotlin
echo "Rust core for $abi: $(du -h "target/android/libs/$abi/libelsewhere.so" | cut -f1)"
