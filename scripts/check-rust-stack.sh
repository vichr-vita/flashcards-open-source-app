#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd -- "$(git rev-parse --show-toplevel)"
if ! node -e 'process.exit(Number(process.versions.node.split(".")[0]) === 24 ? 0 : 1)'; then
  if command -v mise >/dev/null; then
    exec mise exec node@24.21.0 pnpm@12.5.1 -- bash "$script_dir/check-rust-stack.sh" "$@"
  fi
  echo "Install Node 24 before running private-stack checks." >&2
  exit 1
fi
case "${1:-}" in
  "") exec bash "$script_dir/with-rust-test-db.sh" bash "$script_dir/check-rust-stack.sh" --ci ;;
  --ci) ;;
  *) echo "Usage: scripts/check-rust-stack.sh [--ci]" >&2; exit 2 ;;
esac

cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bin lingvichr
export MIGRATION_DATABASE_URL=postgresql://flashcards_owner@127.0.0.1:29432/flashcards
target/debug/lingvichr migrate
# A second run must leave the installed ledger and data intact.
target/debug/lingvichr migrate
export CORE_TEST_DATABASE_URL="$MIGRATION_DATABASE_URL"
export CORE_TEST_BACKEND_URL=postgresql://backend_app@127.0.0.1:29432/flashcards
export CORE_TEST_AUTH_URL=postgresql://auth_app@127.0.0.1:29432/flashcards
cargo test --locked --tests -- --test-threads=1
pnpm generate:types
git diff --exit-code -- apps/web/src/generated
VITE_APP_BASE_URL=http://localhost:19411 VITE_API_BASE_URL=http://localhost:19400/v1 VITE_AUTH_BASE_URL=http://localhost:19401 pnpm build:web
pnpm build:docs
# The TypeScript fixture exercises real Rust HTTP, PostgreSQL, WebAuthn, and browser flows.
RUST_STACK_BINARY=target/debug/lingvichr LOCAL_AUTH_BROWSER_SMOKE=true \
  pnpm --dir apps/auth exec tsx integration/localAuth.ts
pnpm --dir apps/web test:e2e:review-ui
