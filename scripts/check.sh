#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd -- "$(git rev-parse --show-toplevel)"
if ! node -e 'process.exit(Number(process.versions.node.split(".")[0]) === 24 ? 0 : 1)'; then
  if command -v mise >/dev/null; then
    exec mise exec node@24.21.0 -- bash "$script_dir/check.sh" "$@"
  fi
  echo "Install Node 24 before running fork checks." >&2
  exit 1
fi
case "${1:-}" in
  "")
    exec bash "$script_dir/with-test-db.sh" bash "$script_dir/check.sh" --ci
    ;;
  --ci) ;;
  *)
    echo "Usage: scripts/check.sh [--ci]" >&2
    exit 2
    ;;
esac
bash "$script_dir/test-hooks.sh"

npm --prefix apps/auth run build
npm --prefix apps/backend run build
VITE_APP_BASE_URL=http://localhost:19411 VITE_API_BASE_URL=http://localhost:19400/v1 VITE_AUTH_BASE_URL=http://localhost:19401 npm --prefix apps/web run build -- --outDir /tmp/nibomo-local-web
LOCAL_AUTH_BROWSER_SMOKE=true npm --prefix apps/auth run test:local-integration
npm --prefix apps/web run test:e2e:review-ui
