#!/bin/bash
# Start the new desktop application in Tauri development mode.
set -euo pipefail

# Resolve paths from this file, never from the caller's current directory.
SCRIPT_SOURCE="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd -- "$(dirname -- "$SCRIPT_SOURCE")" && pwd -P)"
ROOT_DIR="$SCRIPT_DIR"
FRONTEND_DIR="$ROOT_DIR/frontend"
TAURI_DIR="$ROOT_DIR/frontend/src-tauri"
TAURI_CONFIG="$TAURI_DIR/tauri.conf.json"
TAURI_TARGET_LIMIT_MB="${NFPROGRESS_TAURI_TARGET_MAX_MB:-2048}"

case "$TAURI_TARGET_LIMIT_MB" in
  ''|*[!0-9]*|0)
    echo "NFPROGRESS_TAURI_TARGET_MAX_MB должно быть положительным целым числом." >&2
    exit 2
    ;;
esac
TAURI_TARGET_LIMIT_KB=$((TAURI_TARGET_LIMIT_MB * 1024))

MODE="${1:-}"
DATA_ROOT=""

case "$MODE" in
  "") ;;
  --data-dir)
    if [ "$#" -ne 2 ] || [ -z "${2:-}" ]; then
      echo "Использование: $0 [--fresh|--legacy|--clean|--data-dir PATH|--check]"
      exit 2
    fi
    DATA_ROOT="$2"
    ;;
  --fresh|--legacy|--clean|--check)
    if [ "$#" -ne 1 ]; then
      echo "Использование: $0 [--fresh|--legacy|--clean|--data-dir PATH|--check]"
      exit 2
    fi
    ;;
  *)
    echo "Использование: $0 [--fresh|--legacy|--clean|--data-dir PATH|--check]"
    exit 2
    ;;
esac

if [ "$(uname -s)" != "Darwin" ]; then
  echo "Этот локальный .sh-скрипт предназначен для macOS."
  exit 2
fi
if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
  echo "Для запуска Tauri нужны Rust/Cargo и Node.js."
  exit 1
fi
for required_file in \
  "$TAURI_CONFIG" \
  "$TAURI_DIR/Cargo.toml" \
  "$TAURI_DIR/Cargo.lock" \
  "$TAURI_DIR/build.rs" \
  "$TAURI_DIR/src/main.rs" \
  "$TAURI_DIR/src/lib.rs"; do
  if [ ! -f "$required_file" ]; then
    echo "Не найден файл Tauri-проекта: $required_file" >&2
    echo "Восстановите каталог frontend/src-tauri и повторите запуск." >&2
    exit 1
  fi
done

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
case "$TARGET" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *)
    echo "Неподдерживаемый Rust target: $TARGET"
    exit 1
    ;;
esac

# Serialize users of the shared target through the maintenance runner.
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  exec python3 "$ROOT_DIR/scripts/maintenance/build_cache.py" run --target "$TARGET" -- bash "$ROOT_DIR/Run Tauri.sh" "$@"
fi

LEGACY_TAURI_TARGET_DIR="$TAURI_DIR/target"
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  case "$CARGO_TARGET_DIR" in
    /*) TAURI_TARGET_DIR="$CARGO_TARGET_DIR" ;;
    *) TAURI_TARGET_DIR="$FRONTEND_DIR/$CARGO_TARGET_DIR" ;;
  esac
else
  TAURI_TARGET_DIR="${HOME}/Library/Caches/nfprogress/tauri-dev-target/$TARGET"
fi

clean_cargo_target() {
  local target_dir="$1"
  local manifest_path="${2:-$TAURI_DIR/Cargo.toml}"
  if [ ! -d "$target_dir" ]; then
    return 0
  fi

  echo "Очищаются Tauri/Cargo-артефакты: $target_dir"
  CARGO_TARGET_DIR="$target_dir" cargo clean --manifest-path "$manifest_path"
}

clean_oversized_cargo_target() {
  local target_dir="$1"
  local target_size_kb

  if [ ! -d "$target_dir" ]; then
    return 0
  fi

  target_size_kb="$(du -sk "$target_dir" 2>/dev/null | awk '{print $1}')"
  if [ -n "$target_size_kb" ] && [ "$target_size_kb" -ge "$TAURI_TARGET_LIMIT_KB" ]; then
    echo "Размер Tauri/Cargo-артефактов: ${target_size_kb} KB; лимит: ${TAURI_TARGET_LIMIT_MB} MB."
    echo "Старые debug-артефакты очищаются перед запуском."
    clean_cargo_target "$target_dir"
  fi
}

if [ "$MODE" = "--clean" ]; then
  clean_cargo_target "$TAURI_TARGET_DIR"
  if [ "$TAURI_TARGET_DIR" != "$LEGACY_TAURI_TARGET_DIR" ]; then
    clean_cargo_target "$LEGACY_TAURI_TARGET_DIR"
  fi
  for workspace_arch in arm intel; do
    workspace_target="$ROOT_DIR/.tauri-build-workspaces/$workspace_arch/frontend/src-tauri/target"
    if [ -d "$workspace_target" ]; then
      workspace_manifest="${workspace_target%/target}/Cargo.toml"
      if [ -f "$workspace_manifest" ]; then
        clean_cargo_target "$workspace_target" "$workspace_manifest"
      fi
    fi
  done
  echo "Артефакты Tauri очищены. При следующем запуске они будут созданы заново в $TAURI_TARGET_DIR."
  exit 0
fi

if [ "$MODE" != "--check" ] && lsof -nP -iTCP:5173 -sTCP:LISTEN >/dev/null 2>&1; then
  PORT_PID="$(lsof -nP -iTCP:5173 -sTCP:LISTEN -t | head -n 1)"
  PORT_COMMAND="$(ps -p "$PORT_PID" -o command= 2>/dev/null || true)"
  echo "Порт 5173 уже занят процессом из другого запуска: ${PORT_COMMAND:-PID $PORT_PID}." >&2
  echo "Остановите старый Tauri/Vite и повторите запуск из $ROOT_DIR." >&2
  exit 1
fi

if [ "$MODE" != "--check" ]; then
  if [ "${WORTA_MANAGED_CACHE:-0}" != "1" ]; then
    clean_oversized_cargo_target "$TAURI_TARGET_DIR"
  fi
  if [ "$TAURI_TARGET_DIR" != "$LEGACY_TAURI_TARGET_DIR" ]; then
    clean_oversized_cargo_target "$LEGACY_TAURI_TARGET_DIR"
  fi
fi

if [ ! -d "$FRONTEND_DIR/node_modules" ]; then
  echo "Устанавливаются frontend-зависимости..."
  (cd "$FRONTEND_DIR" && npm ci)
fi

if [ "$MODE" = "--check" ]; then
  echo "Tauri development prerequisites are ready."
  echo "Rust target: $TARGET"
  echo "Cargo target: $TAURI_TARGET_DIR"
  exit 0
fi

if [ -z "$DATA_ROOT" ]; then
  case "$MODE" in
    --fresh)
      DATA_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/nfprogress-tauri-dev.XXXXXX")"
      ;;
    --legacy)
      DATA_ROOT="${HOME}/Documents/nfprogress/test_data"
      ;;
    *)
      DATA_ROOT="${NFPROGRESS_DATA_DIR:-${HOME}/Documents/nfprogress/test_data}"
      ;;
  esac
fi

if [ "$MODE" = "" ] || [ "$MODE" = "--legacy" ]; then
  if [ -z "${NFPROGRESS_DATA_DIR:-}" ] || [ "$MODE" = "--legacy" ]; then
    "$ROOT_DIR/scripts/prepare-tauri-test-data.sh"
  fi
fi

echo "Tauri dev data root: $DATA_ROOT"
echo "Запускается Tauri dev. Это не production-сборка; при первом запуске Cargo может собрать debug-код."
cd "$FRONTEND_DIR"
# Для обычной разработки incremental-артефакты и debug-символы не нужны:
# они со временем раздувают target на несколько гигабайт.
CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}" \
CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}" \
CARGO_TARGET_DIR="$TAURI_TARGET_DIR" \
NFPROGRESS_BUILD_PROFILE=test NFPROGRESS_DATA_DIR="$DATA_ROOT" \
  exec npm run tauri:dev -- --config "$TAURI_CONFIG"
