#!/usr/bin/env bash
# Sign and notarize a macOS app/DMG without storing credentials in the repo.
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "Usage: $0 APP_PATH OUTPUT_DMG [HELPER_PATH]" >&2
  exit 2
fi

APP_PATH="$(cd -- "$(dirname -- "$1")" && pwd -P)/$(basename -- "$1")"
DMG_PATH="$(cd -- "$(dirname -- "$2")" && pwd -P)/$(basename -- "$2")"
HELPER_PATH=""
if [ "$#" -eq 3 ]; then
  HELPER_PATH="$(cd -- "$(dirname -- "$3")" && pwd -P)/$(basename -- "$3")"
fi

: "${APPLE_SIGNING_IDENTITY:?Set APPLE_SIGNING_IDENTITY to a keychain identity.}"
: "${APPLE_NOTARY_PROFILE:?Set APPLE_NOTARY_PROFILE to an existing notarytool keychain profile.}"

if [ ! -d "$APP_PATH" ] || [ "${APP_PATH##*.}" != "app" ]; then
  echo "APP_PATH must point to a .app bundle." >&2
  exit 2
fi

sign_and_verify() {
  local path="$1"
  codesign --force --deep --options runtime --timestamp \
    --sign "$APPLE_SIGNING_IDENTITY" "$path"
  codesign --verify --deep --strict --verbose=2 "$path"
}

sign_and_verify "$APP_PATH"

if [ -n "$HELPER_PATH" ]; then
  if [ ! -f "$HELPER_PATH" ]; then
    echo "HELPER_PATH does not exist: $HELPER_PATH" >&2
    exit 2
  fi
  sign_and_verify "$HELPER_PATH"
fi

STAGING_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nfprogress-signed-dmg.XXXXXX")"
cleanup() {
  rm -rf -- "$STAGING_DIR"
}
trap cleanup EXIT

ditto "$APP_PATH" "$STAGING_DIR/worta.app"
if [ -n "$HELPER_PATH" ]; then
  # Keep the separately signed helper inside the notarized container.  A bare
  # helper copied beside the DMG would be signed but would not be covered by
  # the notarization ticket.
  cp -- "$HELPER_PATH" "$STAGING_DIR/nfprogress-migration-helper"
fi
ln -s /Applications "$STAGING_DIR/Applications"
mkdir -p "$(dirname -- "$DMG_PATH")"
rm -f -- "$DMG_PATH"
hdiutil create -volname "worta" -srcfolder "$STAGING_DIR" \
  -ov -format UDZO "$DMG_PATH"

# The DMG is the downloadable macOS artifact. The app and optional helper are
# signed before packaging; notarization is intentionally fail-closed.
xcrun notarytool submit "$DMG_PATH" \
  --keychain-profile "$APPLE_NOTARY_PROFILE" --wait
xcrun stapler staple "$DMG_PATH"
xcrun stapler validate "$DMG_PATH"
spctl --assess --type open --context context:primary-signature "$DMG_PATH"

echo "Signed and notarized: $DMG_PATH"
