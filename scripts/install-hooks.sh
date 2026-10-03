#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
# An absolute path also works for linked worktrees on older branches.
hook_path="$repo_root/.githooks"
existing=$(git config --get core.hooksPath || true)
if [[ -n $existing && $existing != .githooks && $existing != "$hook_path" ]]; then
  echo "Another hooks directory is configured: $existing. Combine it with .githooks before installing." >&2
  exit 1
fi
git config --local core.hooksPath "$hook_path"
echo "Installed repository hooks for this clone and its linked worktrees."
