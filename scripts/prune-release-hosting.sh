#!/bin/bash
# Retain only the newest published artifacts for each supported platform.
set -euo pipefail

KEEP_COUNT="${NFPROGRESS_RELEASE_KEEP_COUNT:-3}"

SSH_UPLOAD_HOST="${SSH_UPLOAD_HOST:-77.222.62.219}"
SSH_UPLOAD_USER="${SSH_UPLOAD_USER:-nevskyfore}"
SSH_UPLOAD_DIR="${SSH_UPLOAD_DIR:-nfproject/public_html/app}"
SSH_UPLOAD_PORT="${SSH_UPLOAD_PORT:-22}"
SSH_UPLOAD_KEY_PATH="${SSH_UPLOAD_KEY_PATH:-}"
SSH_UPLOAD_STRICT_HOST_KEY_CHECKING="${SSH_UPLOAD_STRICT_HOST_KEY_CHECKING:-}"
if [ -z "$SSH_UPLOAD_KEY_PATH" ] && [ -f "$HOME/.ssh/worta_hosting" ]; then
  SSH_UPLOAD_KEY_PATH="$HOME/.ssh/worta_hosting"
fi

if ! [[ "$KEEP_COUNT" =~ ^[1-9][0-9]*$ ]]; then
  echo "NFPROGRESS_RELEASE_KEEP_COUNT должен быть положительным целым числом." >&2
  exit 2
fi
if [ -z "$SSH_UPLOAD_KEY_PATH" ] || [ ! -f "$SSH_UPLOAD_KEY_PATH" ]; then
  echo "Не найден SSH-ключ для очистки хостинга." >&2
  exit 1
fi

SSH_TARGET="${SSH_UPLOAD_USER}@${SSH_UPLOAD_HOST}"
SSH_OPTS=(-i "$SSH_UPLOAD_KEY_PATH" -p "$SSH_UPLOAD_PORT" -o ConnectTimeout=30)
if [ -n "$SSH_UPLOAD_STRICT_HOST_KEY_CHECKING" ]; then
  SSH_OPTS+=(-o "StrictHostKeyChecking=$SSH_UPLOAD_STRICT_HOST_KEY_CHECKING")
fi

REMOTE_DIR_QUOTED="$(printf '%q' "$SSH_UPLOAD_DIR")"
KEEP_COUNT_QUOTED="$(printf '%q' "$KEEP_COUNT")"

echo "На хостинге сохраняются последние $KEEP_COUNT версии Windows, macOS ARM и macOS Intel."
ssh "${SSH_OPTS[@]}" "$SSH_TARGET" \
  "RELEASE_DIR=$REMOTE_DIR_QUOTED RELEASE_KEEP_COUNT=$KEEP_COUNT_QUOTED sh -s" <<'REMOTE_SCRIPT'
set -eu

cd "$RELEASE_DIR"

prune_versions() {
  index=0
  for artifact in $(ls -1t -- "$@" 2>/dev/null || true); do
    [ -f "$artifact" ] || continue
    index=$((index + 1))
    if [ "$index" -le "$RELEASE_KEEP_COUNT" ]; then
      continue
    fi
    rm -f -- "$artifact" "$artifact.sig"
    printf 'Удалён устаревший релиз: %s\n' "$artifact"
  done
}

# macOS variants are kept separately: each architecture needs three usable
# archives even when a release was built for just one of them.
prune_versions nfprogress-windows-x86_64-*-setup.exe
prune_versions nfprogress-mac-arm-*.zip
prune_versions nfprogress-mac-intel-*.zip
REMOTE_SCRIPT
