#!/usr/bin/env bash
# Stop and remove the cluster scripts/ci/scratch-postgres.sh started.
set -uo pipefail
data="${SCRATCH_PG_DATA:-}"
[ -n "$data" ] && [ -d "$data" ] || exit 0
pg_ctl=$(command -v pg_ctl || ls /opt/homebrew/opt/postgresql@17/bin/pg_ctl /opt/homebrew/bin/pg_ctl 2>/dev/null | head -1)
[ -n "$pg_ctl" ] && "$pg_ctl" -D "$data" -m fast stop >/dev/null 2>&1
rm -rf "$data" "${data%/*}/pg-sock-${data##*/pg-}"
