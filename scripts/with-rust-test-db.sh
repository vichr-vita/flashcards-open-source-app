#!/usr/bin/env bash
set -euo pipefail

cd -- "$(git rev-parse --show-toplevel)"
command -v docker >/dev/null || { echo "Install Docker to run the isolated PostgreSQL integration checks." >&2; exit 1; }
check_container="lingvichr-check-${BASHPID}-${RANDOM}"
cleanup() {
  check_status=$?
  trap - EXIT
  docker rm -f "$check_container" >/dev/null 2>&1 || true
  exit "$check_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Binding a fresh container fails if this port is busy, rather than reusing any database.
docker run -d --name "$check_container" \
  -p 127.0.0.1:29432:5432 \
  -e POSTGRES_USER=flashcards_owner -e POSTGRES_DB=flashcards \
  -e POSTGRES_HOST_AUTH_METHOD=trust \
  -v "$PWD:/workspace:ro" postgres:16 >/dev/null
check_ready=false
for _attempt in $(seq 1 60); do
  if docker exec "$check_container" pg_isready -h 127.0.0.1 -U flashcards_owner -d flashcards >/dev/null; then
    check_ready=true
    break
  fi
  sleep 1
done
[[ "$check_ready" == true ]] || { echo "Disposable PostgreSQL failed to start." >&2; exit 1; }

# Initialize the installed schema first; the Rust runner must reuse its filename ledger.
docker exec -e MIGRATION_DATABASE_URL=postgresql://flashcards_owner@localhost/flashcards \
  "$check_container" bash /workspace/scripts/deploy/migrate.sh
"$@"
