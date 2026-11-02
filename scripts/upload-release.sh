#!/bin/bash
# Upload a locally built release artifact to the public release hosting.
set -euo pipefail

ARTIFACT_PATH="${1:-}"
REMOTE_NAME="${2:-}"

SSH_UPLOAD_HOST="${SSH_UPLOAD_HOST:-77.222.62.219}"
SSH_UPLOAD_USER="${SSH_UPLOAD_USER:-nevskyfore}"
SSH_UPLOAD_DIR="${SSH_UPLOAD_DIR:-nfproject/public_html/app}"
SSH_UPLOAD_PORT="${SSH_UPLOAD_PORT:-22}"
SSH_UPLOAD_KEY_PATH="${SSH_UPLOAD_KEY_PATH:-}"
if [ -z "$SSH_UPLOAD_KEY_PATH" ] && [ -f "$HOME/.ssh/worta_hosting" ]; then
  SSH_UPLOAD_KEY_PATH="$HOME/.ssh/worta_hosting"
fi

if [ -z "$ARTIFACT_PATH" ] || [ ! -f "$ARTIFACT_PATH" ]; then
  echo "Артефакт для загрузки не найден: ${ARTIFACT_PATH:-<пусто>}" >&2
  exit 1
fi
if [ -z "$SSH_UPLOAD_KEY_PATH" ] || [ ! -f "$SSH_UPLOAD_KEY_PATH" ]; then
  echo "Не найден SSH-ключ. Задайте SSH_UPLOAD_KEY_PATH или положите ключ в ~/.ssh/worta_hosting." >&2
  exit 1
fi

SSH_TARGET="${SSH_UPLOAD_USER}@${SSH_UPLOAD_HOST}"
SSH_OPTS=(-i "$SSH_UPLOAD_KEY_PATH" -p "$SSH_UPLOAD_PORT" -o ConnectTimeout=30)
SCP_OPTS=(-i "$SSH_UPLOAD_KEY_PATH" -P "$SSH_UPLOAD_PORT" -o ConnectTimeout=30)
DEST_NAME="${REMOTE_NAME:-$(basename "$ARTIFACT_PATH")}"

echo "Загрузка ${DEST_NAME} на ${SSH_TARGET}:${SSH_UPLOAD_DIR} ..."
ssh "${SSH_OPTS[@]}" "$SSH_TARGET" "mkdir -p '$SSH_UPLOAD_DIR'"
scp "${SCP_OPTS[@]}" "$ARTIFACT_PATH" "$SSH_TARGET:$SSH_UPLOAD_DIR/$DEST_NAME"
echo "Загрузка завершена: ${DEST_NAME}"
