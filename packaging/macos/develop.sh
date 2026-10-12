#!/usr/bin/env bash
# Build a locally signed app with a stable Photos identity (no notarization or DMG).
# Usage: packaging/macos/develop.sh [--skip-build]
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
if [ "${1:-}" != "--skip-build" ]; then
  export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-line-tables-only}"
  (cd "$ROOT" && cargo build --locked -p lightcraft --features heif)
fi
APP="$CARGO_TARGET_DIR/apple-development/LightCraft.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/Licenses"
cp "$CARGO_TARGET_DIR/debug/lightcraft" "$APP/Contents/MacOS/LightCraft"
xcrun swiftc -O -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macosx11.0" \
  "$ROOT/crates/apple/native/AppleBridge.swift" \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
  -Xlinker "$ROOT/crates/apple/native/Info.plist" -o "$APP/Contents/MacOS/lightcraft-apple"
cp "$ROOT/assets/app-icon/lightcraft.icns" "$APP/Contents/Resources/LightCraft.icns"
sed -e "s/@VERSION@/$VERSION-apple-dev/g" -e "s/@SHORT_VERSION@/$VERSION/g" \
  -e "s/@BUILD_SHA@/${LIGHTCRAFT_BUILD_SHA:-unknown}/g" \
  "$ROOT/packaging/macos/Info.plist.in" > "$APP/Contents/Info.plist"
copy_docs "$APP/Contents/Resources/Licenses"
bash "$ROOT/packaging/macos/prepare-sky-model.sh" "$APP/Contents/Resources"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - "$APP/Contents/MacOS/lightcraft-apple"
codesign --force --sign - "$APP/Contents/MacOS/LightCraft"
codesign --force --sign - "$APP"
codesign --verify --strict --deep "$APP"
echo "$APP"
