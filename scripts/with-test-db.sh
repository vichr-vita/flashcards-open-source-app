#!/usr/bin/env bash
set -euo pipefail

# Start a disposable local cluster. Never reuse an inherited database URL.
for tool in initdb pg_ctl createdb python3; do
  command -v "$tool" >/dev/null || { echo "Install PostgreSQL server tools and Python 3 before running checks." >&2; exit 1; }
done
umask 077
check_db_root=$(mktemp -d "${TMPDIR:-/tmp}/nibomo-checks-XXXXXX")
cleanup() {
  check_status=$?
  trap - EXIT
  if [[ -f "$check_db_root/data/postmaster.pid" ]]; then
    pg_ctl -D "$check_db_root/data" -m immediate -w stop >/dev/null || true
  fi
  rm -rf -- "$check_db_root"
  exit "$check_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for check_pg_variable in ${!PG@}; do
  unset "$check_pg_variable"
done
initdb -D "$check_db_root/data" -U flashcards_owner -A trust --no-instructions >/dev/null
check_port=19432
pg_ctl -D "$check_db_root/data" -o "-F -h 127.0.0.1 -p $check_port -k $check_db_root" -l "$check_db_root/server.log" -w start >/dev/null
createdb -h 127.0.0.1 -p "$check_port" -U flashcards_owner flashcards
export MIGRATION_DATABASE_URL="postgresql://flashcards_owner@127.0.0.1:$check_port/flashcards"
unset BACKEND_DB_PASSWORD AUTH_DB_PASSWORD REPORTING_DB_PASSWORD ADMIN_EMAILS
bash scripts/deploy/migrate.sh
"$@"
