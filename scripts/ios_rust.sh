#!/usr/bin/env bash
# Builds the Rust core for iOS and writes its Swift bindings (needs a Mac with Xcode).
#
#   scripts/ios_rust.sh [device|simulator|both] [release|debug]
#
# Run it after `scripts/mobile_python.py ios`, which puts Python.xcframework (which PyO3
# links against) in target/ios. Writes:
#   target/ios/ElsewhereCore.xcframework   the core as a static library, with its C header
#   target/ios/bindings/elsewhere.swift    the Swift bindings for src/api.rs
# ios/project.yml uses both.
set -euo pipefail

which=${1:-both}
profile=${2:-release}
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
python=$root/target/ios/Python.xcframework
case $which in
  device) targets=(aarch64-apple-ios) ;;
  simulator) targets=(aarch64-apple-ios-sim) ;;
  both) targets=(aarch64-apple-ios aarch64-apple-ios-sim) ;;
  *) echo "usage: $0 [device|simulator|both] [release|debug]" >&2; exit 1 ;;
esac

# (No empty arrays: macOS bash 3.2 treats them as unset under set -u.)
flags=(--profile dev)
[ "$profile" = release ] && flags=(--release)
export PYO3_PYTHON=${PYO3_PYTHON:-python3}
export IPHONEOS_DEPLOYMENT_TARGET=16.0
libraries=()
for target in "${targets[@]}"; do
  if [ "$target" = aarch64-apple-ios ]; then slice=ios-arm64; else slice=ios-arm64_x86_64-simulator; fi
  PYO3_CROSS_LIB_DIR=$python/$slice/lib-arm64 \
    cargo rustc --lib --features playlists --target "$target" "${flags[@]}" --crate-type staticlib
  libraries+=("target/$target/$profile/libelsewhere.a")
done

out=target/ios
rm -rf "$out/bindings" "$out/headers" "$out/ElsewhereCore.xcframework"
cargo run --quiet --features bindgen --bin uniffi-bindgen -- \
  generate --library "${libraries[0]}" --language swift --no-format --out-dir "$out/bindings"
# The C header and module map go inside the xcframework, so Swift can `import elsewhereFFI`.
mkdir -p "$out/headers"
mv "$out/bindings/elsewhereFFI.h" "$out/headers/"
mv "$out/bindings/elsewhereFFI.modulemap" "$out/headers/module.modulemap"
args=()
for library in "${libraries[@]}"; do
  args+=(-library "$library" -headers "$out/headers")
done
xcodebuild -create-xcframework "${args[@]}" -output "$out/ElsewhereCore.xcframework" > /dev/null
echo "Rust core for iOS: $(du -sh "$out/ElsewhereCore.xcframework" | cut -f1)"
