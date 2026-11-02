#!/bin/bash
set -euo pipefail

SCRIPT_SOURCE="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd -- "$(dirname -- "$SCRIPT_SOURCE")" && pwd -P)"

TARGETS=("ARM" "Intel")

echo "Tauri-релизные цели: ${TARGETS[*]}"
run_build() {
  local target="$1"
  case "$target" in
    ARM)
      NFPROGRESS_TAURI_RELEASE_UPLOAD=0 "$SCRIPT_DIR/Release Tauri ARM.sh"
      ;;
    Intel)
      NFPROGRESS_TAURI_RELEASE_UPLOAD=0 "$SCRIPT_DIR/Release Tauri Intel.sh"
      ;;
  esac
}

run_build ARM &
ARM_PID=$!
run_build Intel &
INTEL_PID=$!
status=0
wait "$ARM_PID" || status=1
wait "$INTEL_PID" || status=1
[ "$status" -eq 0 ] || exit "$status"

echo "Обе платформы квалифицированы; публикуются проверенные архивы..."
NFPROGRESS_TAURI_REUSE_ARTIFACT=1 NFPROGRESS_TAURI_RELEASE_UPLOAD=1 \
  "$SCRIPT_DIR/Release Tauri ARM.sh"
NFPROGRESS_TAURI_REUSE_ARTIFACT=1 NFPROGRESS_TAURI_RELEASE_UPLOAD=1 \
  "$SCRIPT_DIR/Release Tauri Intel.sh"

echo "✅ Выбранные Tauri-релизы завершены успешно."
