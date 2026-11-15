#!/bin/bash
# Build and qualify a local Tauri release. Legacy hosting publication is an
# explicit transition-only opt-in and is never part of the normal build.
set -euo pipefail

ARCH="${1:-}"
export NFPROGRESS_BUILD_PROFILE=production
SCRIPT_SOURCE="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd -- "$(dirname -- "$SCRIPT_SOURCE")" && pwd -P)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"

case "$ARCH" in
  arm)
    BUILD_DIR="$ROOT_DIR/build-tauri-arm"
    ARTIFACT_PREFIX="nfprogress-tauri-mac-arm"
    ;;
  intel)
    BUILD_DIR="$ROOT_DIR/build-tauri-intel"
    ARTIFACT_PREFIX="nfprogress-tauri-mac-intel"
    ;;
  *)
    echo "Использование: $0 arm|intel"
    exit 2
    ;;
esac

VERSION="$(node "$ROOT_DIR/scripts/sync-tauri-versions.mjs" --version-only)"
ARTIFACT_PATH="$BUILD_DIR/$ARTIFACT_PREFIX-$VERSION.zip"
SOURCE_REVISION="$(git -C "$ROOT_DIR" rev-parse HEAD)"
TARGET="aarch64-apple-darwin"
if [ "$ARCH" = "intel" ]; then
  TARGET="x86_64-apple-darwin"
fi

if [ "${NFPROGRESS_TAURI_REUSE_ARTIFACT:-0}" = "1" ] && [ -f "$ARTIFACT_PATH" ]; then
  echo "Переиспользуется явно разрешённый Tauri-архив: $ARTIFACT_PATH"
else
  NFPROGRESS_TAURI_RUN_FRONTEND_CHECKS=1 \
  NFPROGRESS_TAURI_RUN_RUST_CHECKS=1 \
  NFPROGRESS_TAURI_KEEP_DMG=1 \
  NFPROGRESS_TAURI_KEEP_BUNDLE=1 \
    "$ROOT_DIR/scripts/build-tauri-local.sh" "$ARCH"
fi

python3 "$ROOT_DIR/scripts/verify-tauri-artifact.py" "$ARTIFACT_PATH" "$SOURCE_REVISION"

APP_PATH="$ROOT_DIR/.tauri-build-workspaces/$ARCH/frontend/src-tauri/target/$TARGET/release/bundle/macos/worta.app"
DMG_PATH="$BUILD_DIR/$ARTIFACT_PREFIX-$VERSION.dmg"
if [ ! -d "$APP_PATH" ] || [ ! -f "$DMG_PATH" ]; then
  echo "Release qualification requires the retained app and DMG: $APP_PATH; $DMG_PATH" >&2
  exit 1
fi
"$ROOT_DIR/scripts/verify-tauri-macos-app.sh" "$APP_PATH" "$TARGET"

if command -v shasum >/dev/null 2>&1; then
  sha256_file() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  sha256_file() { sha256sum "$1" | awk '{print $1}'; }
fi

if python3 - "$ROOT_DIR/frontend/src-tauri/tauri.conf.json" <<'PY'
import json
import sys

config = json.load(open(sys.argv[1], encoding="utf-8"))
updater = config.get("plugins", {}).get("updater", {})
raise SystemExit(0 if updater.get("pubkey") and updater.get("endpoints") else 1)
PY
then
  UPDATER_STATUS="enabled"
else
  UPDATER_STATUS="disabled"
fi

WORKTREE_STATUS="clean"
if ! git -C "$ROOT_DIR" diff --quiet; then
  WORKTREE_STATUS="dirty (not included in commit SHA)"
fi

printf '\n=== Tauri macOS release qualification ===\n'
printf 'Версия: %s\n' "$VERSION"
printf 'Commit SHA: %s\n' "$SOURCE_REVISION"
printf 'Рабочее дерево: %s\n' "$WORKTREE_STATUS"
printf 'Архитектура: %s\n' "$TARGET"
printf 'App: %s (%s)\n' "$APP_PATH" "$(du -sh "$APP_PATH" | awk '{print $1}')"
printf 'DMG: %s (%s, SHA-256 %s)\n' "$DMG_PATH" "$(du -h "$DMG_PATH" | awk '{print $1}')" "$(sha256_file "$DMG_PATH")"
printf 'Release ZIP: %s (%s, SHA-256 %s)\n' "$ARTIFACT_PATH" "$(du -h "$ARTIFACT_PATH" | awk '{print $1}')" "$(sha256_file "$ARTIFACT_PATH")"
printf 'OS signing/notarization: unsigned (optional gate not requested)\n'
printf 'Tauri updater: %s\n' "$UPDATER_STATUS"
printf 'Qualification: PASS; publication: %s\n' "${NFPROGRESS_TAURI_RELEASE_UPLOAD:-0} (explicit legacy-transition opt-in only)"

echo "Локальный Tauri-архив готов: $ARTIFACT_PATH"

if [ "${NFPROGRESS_TAURI_RELEASE_UPLOAD:-0}" != "1" ]; then
  echo "Публикация отключена. Для legacy transition hosting задайте NFPROGRESS_TAURI_RELEASE_UPLOAD=1 явно."
  exit 0
fi

MANIFEST_PATH="$(mktemp "${TMPDIR:-/tmp}/nfprogress-update-manifest.XXXXXX")"
WORTA_MANIFEST_PATH="$(mktemp "${TMPDIR:-/tmp}/worta-update-manifest.XXXXXX")"
MANIFEST_LOCK_DIR="$ROOT_DIR/.release-tauri-manifest.lock"
REMOTE_MANIFEST_PATH="$(mktemp "${TMPDIR:-/tmp}/nfprogress-manifest.XXXXXX")"
REMOTE_RELEASES_PATH="$(mktemp "${TMPDIR:-/tmp}/worta-releases.XXXXXX")"

while ! mkdir "$MANIFEST_LOCK_DIR" 2>/dev/null; do
  lock_pid=""
  if [ -f "$MANIFEST_LOCK_DIR/pid" ] \
    && read -r lock_pid < "$MANIFEST_LOCK_DIR/pid" \
    && [[ "$lock_pid" =~ ^[0-9]+$ ]] \
    && ! kill -0 "$lock_pid" 2>/dev/null; then
    echo "Удаляется блокировка манифеста от завершившегося релиза (PID $lock_pid)."
    rm -f -- "$MANIFEST_LOCK_DIR/pid"
    rmdir "$MANIFEST_LOCK_DIR" 2>/dev/null || true
    continue
  fi
  echo "Ожидается обновление общего манифеста другим macOS-релизом..."
  sleep 1
done
printf '%s\n' "$$" > "$MANIFEST_LOCK_DIR/pid"

cleanup_release_manifest() {
  rm -f -- "$REMOTE_MANIFEST_PATH"
  if [ -n "${LEGACY_DIR:-}" ]; then rm -rf -- "$LEGACY_DIR"; fi
  if [ -n "${RELEASES_PATH:-}" ]; then rm -f -- "$RELEASES_PATH"; fi
  rm -f -- "$MANIFEST_PATH" "$WORTA_MANIFEST_PATH" "$REMOTE_RELEASES_PATH"
  rm -f -- "$MANIFEST_LOCK_DIR/pid"
  rmdir "$MANIFEST_LOCK_DIR" 2>/dev/null || true
}
interrupt_release_manifest() {
  cleanup_release_manifest
  exit 130
}
trap cleanup_release_manifest EXIT
trap interrupt_release_manifest HUP INT TERM

if curl --fail --location --retry 3 --silent --show-error \
  --output "$REMOTE_MANIFEST_PATH" \
  "https://nfproject.ru/app/update_manifest.json"; then
  mv "$REMOTE_MANIFEST_PATH" "$MANIFEST_PATH"
else
  echo "Удалённый манифест недоступен; используется локальная копия."
  if [ -f "$ROOT_DIR/update_manifest.json" ]; then
    cp "$ROOT_DIR/update_manifest.json" "$MANIFEST_PATH"
  else
    printf '%s\n' '{}' > "$MANIFEST_PATH"
  fi
fi

if curl --fail --location --retry 3 --silent --show-error \
  --output "$WORTA_MANIFEST_PATH" \
  "https://worta.ru/app/update_manifest.json"; then
  :
else
  echo "Манифест worta.ru недоступен; используется манифест nfprogress.ru."
  cp "$MANIFEST_PATH" "$WORTA_MANIFEST_PATH"
fi

export RELEASE_NOTES="${RELEASE_NOTES:-}"
RELEASE_BASE_URL="https://worta.ru/app" \
RELEASE_MANIFEST_PATH="$MANIFEST_PATH" \
  python3 "$ROOT_DIR/scripts/update-release-manifest.py" "$VERSION" "macos_${ARCH}" "$ARTIFACT_PATH"
RELEASE_BASE_URL="https://worta.ru/app" \
RELEASE_MANIFEST_PATH="$WORTA_MANIFEST_PATH" \
  python3 "$ROOT_DIR/scripts/update-release-manifest.py" "$VERSION" "macos_${ARCH}" "$ARTIFACT_PATH"
python3 - "$MANIFEST_PATH" "macos_${ARCH}" <<'PY'
import json
import sys

manifest_path, platform = sys.argv[1:]
manifest = json.load(open(manifest_path, encoding="utf-8"))
release = manifest.get(platform, {})
if not isinstance(release, dict) or not release.get("sha256") or not release.get("size"):
    raise SystemExit(f"В манифесте нет sha256/size для {platform}.")
PY
python3 - "$WORTA_MANIFEST_PATH" "macos_${ARCH}" <<'PY'
import json
import sys

manifest_path, platform = sys.argv[1:]
manifest = json.load(open(manifest_path, encoding="utf-8"))
release = manifest.get(platform, {})
if not isinstance(release, dict) or not release.get("sha256") or not release.get("size"):
    raise SystemExit(f"В манифесте нет sha256/size для {platform}.")
PY

if [ "${NFPROGRESS_TAURI_RELEASE_UPLOAD:-1}" = "1" ]; then
  REMOTE_NAME="nfprogress-mac-${ARCH}-${VERSION}.zip"
  LEGACY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nfprogress-legacy-manifest.XXXXXX")"
  cp "$MANIFEST_PATH" "$LEGACY_DIR/update_manifest.json"
  (cd "$LEGACY_DIR" && python3 "$ROOT_DIR/scripts/create-legacy-manifest.py")

  export SSH_UPLOAD_DIR="/home/n/nevskyfore/nfproject/public_html/app"
  "$ROOT_DIR/scripts/upload-release.sh" "$MANIFEST_PATH" "update_manifest.json"
  "$ROOT_DIR/scripts/upload-release.sh" "$LEGACY_DIR/update_manifest_legacy.json" "update_manifest_legacy.json"

  export SSH_UPLOAD_DIR="/home/n/nevskyfore/worta/public_html/app"
  "$ROOT_DIR/scripts/upload-release.sh" "$ARTIFACT_PATH" "$REMOTE_NAME"
  "$ROOT_DIR/scripts/upload-release.sh" "$WORTA_MANIFEST_PATH" "update_manifest.json"

  if curl --fail --location --retry 3 --silent --show-error \
    --output "$REMOTE_RELEASES_PATH" "https://worta.ru/releases.json"; then
    :
  else
    printf '%s\n' '{"schema_version":1,"releases":[]}' > "$REMOTE_RELEASES_PATH"
  fi
  RELEASES_PATH="$(mktemp "${TMPDIR:-/tmp}/worta-releases-updated.XXXXXX")"
  python3 "$ROOT_DIR/scripts/create-worta-releases-manifest.py" \
    --version "$VERSION" \
    --source "$REMOTE_RELEASES_PATH" \
    --platform "$([ "$ARCH" = arm ] && echo macos-arm || echo macos-intel)" \
    --output "$RELEASES_PATH"
  export SSH_UPLOAD_DIR="/home/n/nevskyfore/worta/public_html"
  "$ROOT_DIR/scripts/upload-release.sh" "$RELEASES_PATH" "releases.json"
else
  echo "Загрузка на хостинг отключена: NFPROGRESS_TAURI_RELEASE_UPLOAD=0"
fi
