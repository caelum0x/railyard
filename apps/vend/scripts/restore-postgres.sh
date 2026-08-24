#!/usr/bin/env bash
set -euo pipefail

required=(DATABASE_URL VEND_BACKUP_FILE VEND_RESTORE_DATABASE VEND_RESTORE_CONFIRM)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    echo "vend restore: required environment variable $name is missing" >&2
    exit 64
  fi
done
for command in psql pg_restore; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "vend restore: required command '$command' is unavailable" >&2
    exit 69
  fi
done
if [[ ! -f "$VEND_BACKUP_FILE" ]]; then
  echo "vend restore: backup file does not exist: $VEND_BACKUP_FILE" >&2
  exit 66
fi
if [[ "$VEND_RESTORE_CONFIRM" != "restore:$VEND_RESTORE_DATABASE" ]]; then
  echo "vend restore: set VEND_RESTORE_CONFIRM=restore:$VEND_RESTORE_DATABASE" >&2
  exit 64
fi

actual_database="$(psql "$DATABASE_URL" -X --set=ON_ERROR_STOP=1 --tuples-only --no-align --command='SELECT current_database()')"
if [[ "$actual_database" != "$VEND_RESTORE_DATABASE" ]]; then
  echo "vend restore: connected database '$actual_database' does not match VEND_RESTORE_DATABASE" >&2
  exit 64
fi

pg_restore --list "$VEND_BACKUP_FILE" >/dev/null
pg_restore "$VEND_BACKUP_FILE" \
  --dbname="$DATABASE_URL" \
  --clean \
  --if-exists \
  --no-owner \
  --no-privileges \
  --exit-on-error
psql "$DATABASE_URL" -X --set=ON_ERROR_STOP=1 \
  --command='SELECT version, success, installed_on FROM _sqlx_migrations ORDER BY version'
echo "vend restore: restored and migration inventory is readable"
