#!/usr/bin/env bash
# Builds the Rust mobile bridge (crates/mobile) as ios/Frameworks/EmberCore.xcframework and generates
# its Swift bindings into ios/Ember/Generated/. Both outputs are gitignored build products.
#
# Runs on macOS with Xcode and the aarch64-apple-ios / aarch64-apple-ios-sim Rust targets:
#   scripts/ios/build-xcframework.sh            # dev profile (local simulator work)
#   PROFILE=release scripts/ios/build-xcframework.sh
# Then: cd ios && xcodegen generate && xcodebuild ...
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="${PROFILE:-dev}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
PROFILE_DIR="$PROFILE"
[ "$PROFILE" = "dev" ] && PROFILE_DIR="debug"

LIB=liblightcraft_mobile.a
DEVICE=aarch64-apple-ios
SIM=aarch64-apple-ios-sim
HOST=aarch64-apple-darwin
OUT="$ROOT/ios/Frameworks/EmberCore.xcframework"
GEN="$ROOT/ios/Ember/Generated"
WORK="$TARGET_DIR/ember-ios-bindings"

cd "$ROOT"
for target in "$DEVICE" "$SIM" "$HOST"; do
  echo "==> cargo build lightcraft-mobile ($target, $PROFILE)"
  IPHONEOS_DEPLOYMENT_TARGET=17.0 cargo build -p lightcraft-mobile --lib --profile "$PROFILE" --target "$target"
done

echo "==> generate Swift bindings"
rm -rf "$WORK" && mkdir -p "$WORK/headers" "$GEN"
cargo run -q -p lightcraft-mobile --features bindgen --bin uniffi-bindgen -- \
  generate --library "$TARGET_DIR/$HOST/$PROFILE_DIR/$LIB" --language swift --out-dir "$WORK/swift"
cp "$WORK/swift/"*.h "$WORK/headers/"
# The XCFramework's headers directory needs the module map under its canonical name.
cp "$WORK/swift/"*.modulemap "$WORK/headers/module.modulemap"
cp "$WORK/swift/"*.swift "$GEN/"

echo "==> create $OUT"
rm -rf "$OUT"
mkdir -p "$(dirname "$OUT")"
xcodebuild -create-xcframework \
  -library "$TARGET_DIR/$DEVICE/$PROFILE_DIR/$LIB" -headers "$WORK/headers" \
  -library "$TARGET_DIR/$SIM/$PROFILE_DIR/$LIB" -headers "$WORK/headers" \
  -output "$OUT"
echo "==> done: $OUT"
