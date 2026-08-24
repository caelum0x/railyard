#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${DATABASE_URL:-}" || -z "${VEND_BACKUP_FILE:-}" ]]; then
  echo "usage: DATABASE_URL=... VEND_BACKUP_FILE=/absolute/path/vend.dump $0" >&2
  exit 64
fi
for command in pg_dump pg_restore; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "vend backup: required command '$command' is unavailable" >&2
    exit 69
  fi
done
if [[ "$VEND_BACKUP_FILE" != /* ]]; then
  echo "vend backup: VEND_BACKUP_FILE must be an absolute path" >&2
  exit 64
fi
if [[ -e "$VEND_BACKUP_FILE" ]]; then
  echo "vend backup: refusing to overwrite existing file $VEND_BACKUP_FILE" >&2
  exit 73
fi

umask 077
pg_dump "$DATABASE_URL" \
  --format=custom \
  --compress=9 \
  --no-owner \
  --no-privileges \
  --file="$VEND_BACKUP_FILE"
pg_restore --list "$VEND_BACKUP_FILE" >/dev/null
echo "vend backup: verified $VEND_BACKUP_FILE"
