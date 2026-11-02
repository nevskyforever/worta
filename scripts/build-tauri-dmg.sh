#!/bin/bash
# Build a plain distributable DMG without Finder/AppleScript layout automation.
set -euo pipefail

SCRIPT_SOURCE="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd -- "$(dirname -- "$SCRIPT_SOURCE")" && pwd -P)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
TARGET="${1:-}"
FRONTEND_DIR="${NFPROGRESS_TAURI_FRONTEND_DIR:-$ROOT_DIR/frontend}"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "This script creates a macOS DMG and must run on macOS."
  exit 2
fi

case "$TARGET" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *)
    echo "Usage: $0 aarch64-apple-darwin|x86_64-apple-darwin [output.dmg]"
    exit 2
    ;;
esac

FRONTEND_DIR="$(cd -- "$FRONTEND_DIR" && pwd -P)"
APP_PATH="$FRONTEND_DIR/src-tauri/target/$TARGET/release/bundle/macos/worta.app"
node "$ROOT_DIR/scripts/sync-tauri-versions.mjs" --frontend-dir "$FRONTEND_DIR"
VERSION="$(node "$ROOT_DIR/scripts/sync-tauri-versions.mjs" --version-only)"
OUTPUT_PATH="${2:-$FRONTEND_DIR/src-tauri/target/$TARGET/release/bundle/dmg/nfprogress-$VERSION-$TARGET.dmg}"

cd "$FRONTEND_DIR"
CARGO_TARGET_DIR="$FRONTEND_DIR/src-tauri/target" \
  npx tauri build --target "$TARGET" --bundles app

if [ ! -d "$APP_PATH" ]; then
  echo "Tauri did not produce the expected app bundle: $APP_PATH"
  exit 1
fi

"$ROOT_DIR/scripts/verify-tauri-macos-app.sh" "$APP_PATH" "$TARGET"

mkdir -p "$(dirname "$OUTPUT_PATH")"
STAGING_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nfprogress-tauri-dmg.XXXXXX")"
trap 'rm -rf -- "$STAGING_DIR"' EXIT

ditto "$APP_PATH" "$STAGING_DIR/worta.app"
ln -s /Applications "$STAGING_DIR/Applications"
hdiutil create -volname "worta" -srcfolder "$STAGING_DIR" -ov -format UDZO "$OUTPUT_PATH"
hdiutil verify "$OUTPUT_PATH"

echo "$OUTPUT_PATH"
