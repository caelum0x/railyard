#!/usr/bin/env bash
# Start a throwaway Postgres cluster for one CI job and export DATABASE_URL.
#
#   scripts/ci/scratch-postgres.sh <database-name>
#
# Uses the runner's own initdb/pg_ctl (Homebrew on the Mac runner); the data
# directory lives under $RUNNER_TEMP (or a temp dir) and is removed by
# scripts/ci/scratch-postgres-stop.sh. Never touches a hosted database. When
# initdb is not installed it prints a notice and exports nothing, so the
# database-backed suites stay skipped instead of failing.
set -euo pipefail

db="${1:?database name}"
find_bin() {
  if command -v "$1" >/dev/null 2>&1; then command -v "$1"; return; fi
  for d in /opt/homebrew/opt/postgresql@17/bin /opt/homebrew/opt/postgresql@16/bin /opt/homebrew/bin /usr/lib/postgresql/17/bin /usr/lib/postgresql/16/bin; do
    [ -x "$d/$1" ] && { echo "$d/$1"; return; }
  done
  return 1
}
if ! initdb=$(find_bin initdb) || ! pg_ctl=$(find_bin pg_ctl) || ! createdb=$(find_bin createdb); then
  echo "::notice::initdb/pg_ctl not installed on this runner; database-backed suites stay skipped"
  exit 0
fi

base="${RUNNER_TEMP:-$(mktemp -d)}"
data="$base/pg-$db"
sock="$base/pg-sock-$db"
rm -rf "$data" "$sock"
mkdir -p "$sock"
# A free port: let the OS choose, then hand it to Postgres.
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')

"$initdb" -D "$data" -U postgres -A trust >/dev/null
"$pg_ctl" -D "$data" -l "$data/server.log" -w \
  -o "-p $port -k $sock -c listen_addresses=127.0.0.1 -c fsync=off -c full_page_writes=off -c max_connections=300" start >/dev/null
"$createdb" -h 127.0.0.1 -p "$port" -U postgres "$db"

url="postgres://postgres@127.0.0.1:$port/$db"
echo "scratch Postgres for $db on port $port"
if [ -n "${GITHUB_ENV:-}" ]; then
  {
    echo "DATABASE_URL=$url"
    # Suites that read their own variable first (they share the database).
    echo "TURNSTILE_DATABASE_URL=$url"
    echo "RELAY_TEST_DATABASE_URL=$url"
    echo "SCRATCH_PG_DATA=$data"
  } >>"$GITHUB_ENV"
else
  echo "DATABASE_URL=$url"
fi
