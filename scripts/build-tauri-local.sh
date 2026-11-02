#!/bin/bash
# Build a local macOS Tauri archive.
set -euo pipefail

ARCH="${1:-}"
BUILD_PROFILE="${NFPROGRESS_BUILD_PROFILE:-production}"
TAURI_TARGET_LIMIT_MB="${NFPROGRESS_TAURI_TARGET_MAX_MB:-2048}"
SCRIPT_SOURCE="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd -- "$(dirname -- "$SCRIPT_SOURCE")" && pwd -P)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"

case "$TAURI_TARGET_LIMIT_MB" in
  ''|*[!0-9]*|0)
    echo "NFPROGRESS_TAURI_TARGET_MAX_MB должно быть положительным целым числом." >&2
    exit 2
    ;;
esac
TAURI_TARGET_LIMIT_KB=$((TAURI_TARGET_LIMIT_MB * 1024))
case "$ARCH" in
  arm)
    TARGET="aarch64-apple-darwin"
    BUILD_DIR="$ROOT_DIR/build-tauri-arm"
    ARTIFACT_PREFIX="nfprogress-tauri-mac-arm"
    ;;
  intel)
    TARGET="x86_64-apple-darwin"
    BUILD_DIR="$ROOT_DIR/build-tauri-intel"
    ARTIFACT_PREFIX="nfprogress-tauri-mac-intel"
    ;;
  *)
    echo "Использование: $0 arm|intel"
    exit 2
    ;;
esac

case "$BUILD_PROFILE" in
  test)
    ARTIFACT_PREFIX="nfprogress-test-tauri-mac-$ARCH"
    ;;
  production)
    ;;
  *)
    echo "Неподдерживаемый NFPROGRESS_BUILD_PROFILE: $BUILD_PROFILE"
    echo "Ожидается: test или production"
    exit 2
    ;;
esac
export NFPROGRESS_BUILD_PROFILE="$BUILD_PROFILE"

WORKSPACE_DIR="$ROOT_DIR/.tauri-build-workspaces/$ARCH"
FRONTEND_SOURCE_DIR="$ROOT_DIR/frontend"
FRONTEND_DIR="$WORKSPACE_DIR/frontend"
WORKSPACE_TARGET_DIR="$FRONTEND_DIR/src-tauri/target"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "Локальная Tauri-сборка macOS должна выполняться на macOS."
  exit 2
fi

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
  echo "Не найдены Cargo/Rustup. Установите Rust toolchain для Tauri."
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "Не найден npm. Установите Node.js 20.19 или новее."
  exit 1
fi
if ! command -v rsync >/dev/null 2>&1; then
  echo "Не найден rsync. Он нужен для изолированной Tauri-сборки."
  exit 1
fi
if ! rustup target list --installed | grep -Fxq "$TARGET"; then
  echo "Не установлен Rust target $TARGET. Выполните:"
  echo "  rustup target add $TARGET"
  exit 1
fi

if [ "$BUILD_PROFILE" = "test" ]; then
  "$ROOT_DIR/scripts/prepare-tauri-test-data.sh"
fi

prepare_frontend_workspace() {
  echo "Подготавливается изолированный frontend-workspace для $ARCH-сборки..."
  mkdir -p "$WORKSPACE_DIR"
  rsync --archive --delete \
    --exclude '.DS_Store' \
    --exclude 'node_modules/' \
    --exclude 'dist/' \
    --exclude '.vite/' \
    --exclude 'coverage/' \
    --exclude 'public/mindmap-assets/' \
    --exclude 'src-tauri/target/' \
    --exclude 'src-tauri/binaries/' \
    "$FRONTEND_SOURCE_DIR/" "$FRONTEND_DIR/"
  rsync --archive --delete \
    "$ROOT_DIR/mindmap_assets/" "$WORKSPACE_DIR/mindmap_assets/"
  # Vitest's node-only release checks resolve shared compression fixtures and
  # the local FastAPI app from the workspace root. Copy only their source
  # trees; these are test inputs and are never included in the Tauri bundle.
  if [ -d "$ROOT_DIR/scripts/compression" ]; then
    mkdir -p "$WORKSPACE_DIR/scripts"
    rsync --archive --delete \
      "$ROOT_DIR/scripts/compression/" "$WORKSPACE_DIR/scripts/compression/"
  fi
  if [ -d "$ROOT_DIR/backend" ]; then
    rsync --archive --delete \
      "$ROOT_DIR/backend/" "$WORKSPACE_DIR/backend/"
  fi
  # The backend smoke test imports the shared service package and legacy
  # engine module from the repository root. They remain test-only sources and
  # are not part of the production Desktop artifact.
  if [ -d "$ROOT_DIR/nfprogress" ]; then
    rsync --archive --delete \
      "$ROOT_DIR/nfprogress/" "$WORKSPACE_DIR/nfprogress/"
  fi
  if [ -f "$ROOT_DIR/engine.py" ]; then
    cp -p "$ROOT_DIR/engine.py" "$WORKSPACE_DIR/engine.py"
  fi
  # Rust embeds the shared migration SQL from the repository-relative path in
  # src-tauri/src/sqlite.rs. Keep that source available in the isolated build
  # workspace without copying the Python runtime or its user data.
  if [ -d "$ROOT_DIR/nfprogress/core/sqlite/migrations" ]; then
    mkdir -p "$WORKSPACE_DIR/nfprogress/core/sqlite"
    rsync --archive --delete \
      "$ROOT_DIR/nfprogress/core/sqlite/migrations/" \
      "$WORKSPACE_DIR/nfprogress/core/sqlite/migrations/"
  fi
  cp -p \
    "$ROOT_DIR/Icon-256.png" \
    "$ROOT_DIR/appicon.icns" \
    "$ROOT_DIR/icon.ico" \
    "$WORKSPACE_DIR/"
}

prepare_frontend_workspace

if [ -d "$WORKSPACE_TARGET_DIR" ]; then
  WORKSPACE_TARGET_SIZE_KB="$(du -sk "$WORKSPACE_TARGET_DIR" 2>/dev/null | awk '{print $1}')"
  if [ -n "$WORKSPACE_TARGET_SIZE_KB" ] \
    && [ "$WORKSPACE_TARGET_SIZE_KB" -ge "$TAURI_TARGET_LIMIT_KB" ]; then
    echo "Размер Tauri build target: ${WORKSPACE_TARGET_SIZE_KB} KB; лимит: ${TAURI_TARGET_LIMIT_MB} MB."
    echo "Старый target изолированной сборки очищается перед запуском."
    CARGO_TARGET_DIR="$WORKSPACE_TARGET_DIR" \
      cargo clean --manifest-path "$FRONTEND_DIR/src-tauri/Cargo.toml"
  fi
fi

NODE_MODULES_LOCK="$FRONTEND_DIR/node_modules/.package-lock.json"
if [ ! -f "$NODE_MODULES_LOCK" ] \
  || [ "$FRONTEND_DIR/package.json" -nt "$NODE_MODULES_LOCK" ] \
  || [ "$FRONTEND_DIR/package-lock.json" -nt "$NODE_MODULES_LOCK" ]; then
  echo "Устанавливаются или обновляются frontend-зависимости..."
  (cd "$FRONTEND_DIR" && npm ci)
fi

node "$ROOT_DIR/scripts/sync-tauri-versions.mjs" --frontend-dir "$FRONTEND_DIR"
node "$ROOT_DIR/scripts/apply-tauri-build-profile.mjs" \
  "$FRONTEND_DIR/src-tauri/tauri.conf.json" "$BUILD_PROFILE"
VERSION="$(node "$ROOT_DIR/scripts/sync-tauri-versions.mjs" --version-only)"
DMG_PATH="$BUILD_DIR/$ARTIFACT_PREFIX-$VERSION.dmg"
ARTIFACT_PATH="$BUILD_DIR/$ARTIFACT_PREFIX-$VERSION.zip"
PACKAGE_NAME="$ARTIFACT_PREFIX-$VERSION"

if [ "${NFPROGRESS_TAURI_RUN_FRONTEND_CHECKS:-0}" = "1" ]; then
  echo "Запускаются frontend typecheck и tests для release qualification..."
  (cd "$FRONTEND_DIR" && npm run typecheck && npm run test)
fi

if [ "${NFPROGRESS_TAURI_RUN_RUST_CHECKS:-0}" = "1" ]; then
  echo "Запускаются Rust fmt/check/tests для release qualification..."
  cargo fmt --manifest-path "$FRONTEND_DIR/src-tauri/Cargo.toml" --check
  cargo check \
    --manifest-path "$FRONTEND_DIR/src-tauri/Cargo.toml" \
    --target "$TARGET"
  cargo test \
    --manifest-path "$FRONTEND_DIR/src-tauri/Cargo.toml" \
    --target "$TARGET"
fi

mkdir -p "$BUILD_DIR"
NFPROGRESS_TAURI_FRONTEND_DIR="$FRONTEND_DIR" \
  "$ROOT_DIR/scripts/build-tauri-dmg.sh" "$TARGET" "$DMG_PATH"

STAGING_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nfprogress-tauri-package.XXXXXX")"
trap 'rm -rf -- "$STAGING_DIR"' EXIT
PACKAGE_DIR="$STAGING_DIR/$PACKAGE_NAME"
mkdir -p "$PACKAGE_DIR"
cp "$DMG_PATH" "$PACKAGE_DIR/"
cp "$ROOT_DIR/LICENSE" "$PACKAGE_DIR/LICENSE.txt"
cp "$ROOT_DIR/SOURCE_CODE.txt" "$PACKAGE_DIR/SOURCE_CODE.txt"
SOURCE_REVISION="$(git -C "$ROOT_DIR" rev-parse HEAD)"
{
  printf '\nРевизия сборки: %s\n' "$SOURCE_REVISION"
  printf 'Архив исходного кода: https://github.com/nevskyforever/worta/archive/%s.zip\n' "$SOURCE_REVISION"
} >> "$PACKAGE_DIR/SOURCE_CODE.txt"

rm -f "$ARTIFACT_PATH"
ditto -c -k --norsrc --keepParent "$PACKAGE_DIR" "$ARTIFACT_PATH"

# The ZIP is the portable release package. Release qualification may retain the
# DMG and app bundle for direct inspection; ordinary local builds keep the
# historical compact output.
if [ "${NFPROGRESS_TAURI_KEEP_DMG:-0}" != "1" ]; then
  rm -f -- "$DMG_PATH"
fi
find "$BUILD_DIR" -mindepth 1 -maxdepth 1 -type f \
  -name "$ARTIFACT_PREFIX-*" \
  ! -name "$(basename "$ARTIFACT_PATH")" \
  ! -name "$(basename "$DMG_PATH")" \
  -delete

# Keep dependency caches in the isolated workspace, but not the completed app
# bundle that is already contained in the release ZIP.
if [ "${NFPROGRESS_TAURI_KEEP_BUNDLE:-0}" != "1" ]; then
  rm -rf -- "$FRONTEND_DIR/src-tauri/target/$TARGET/release/bundle"
fi

# A regular local test build only needs the ZIP in build-tauri-*/. Keep the
# large Rust target only for explicit release qualification, which inspects the
# retained app bundle after this script returns.
if [ "${NFPROGRESS_TAURI_KEEP_BUILD_CACHE:-0}" != "1" ] \
  && [ "${NFPROGRESS_TAURI_KEEP_BUNDLE:-0}" != "1" ]; then
  echo "Очищается временный Tauri build target: $WORKSPACE_TARGET_DIR"
  CARGO_TARGET_DIR="$WORKSPACE_TARGET_DIR" \
    cargo clean --manifest-path "$FRONTEND_DIR/src-tauri/Cargo.toml"
fi

echo "✅ Локальная Tauri-сборка завершена: $ARTIFACT_PATH"
