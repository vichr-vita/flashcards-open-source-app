#!/usr/bin/env bash
set -euo pipefail

# Exercise Git's real pre-push lifecycle against a disposable local remote.
source_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/pre-push-test-XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
while IFS= read -r check_git_variable; do
  unset "$check_git_variable"
done < <(git rev-parse --local-env-vars)
git init --quiet --bare "$test_root/remote.git"
git init --quiet "$test_root/checkout"
cd "$test_root/checkout"
git config user.name "Hook test"
git config user.email "hook-test@example.invalid"
git remote add origin "$test_root/remote.git"
mkdir .githooks scripts
cp "$source_root/.githooks/pre-push" .githooks/pre-push
cat > scripts/check.sh <<'CHECK'
#!/usr/bin/env bash
set -euo pipefail
case "${HOOK_TEST_RESULT:-pass}" in
  fail) exit 1 ;;
  change) echo changed >> tracked.txt ;;
  *) git rev-parse HEAD > "$HOOK_TEST_RECORD" ;;
esac
CHECK
echo initial > tracked.txt
git add .
git -c core.hooksPath=/dev/null commit --quiet -m "Initial fixture"
git config core.hooksPath .githooks
export HOOK_TEST_RECORD="$test_root/checked-commit"
export HOOK_TEST_RESULT=pass

expect_rejected() {
  if git push "$@" > "$test_root/push.log" 2>&1; then
    echo "Expected push rejection: $*" >&2
    exit 1
  fi
}

git push --quiet origin HEAD:refs/heads/test
[[ $(cat "$HOOK_TEST_RECORD") == "$(git rev-parse HEAD)" ]]
first=$(git rev-parse HEAD)

echo dirty >> tracked.txt
expect_rejected origin HEAD:refs/heads/dirty
git restore tracked.txt
touch untracked.txt
expect_rejected origin HEAD:refs/heads/untracked
rm untracked.txt

export HOOK_TEST_RESULT=fail
expect_rejected origin HEAD:refs/heads/failing
[[ -z $(git ls-remote origin refs/heads/failing) ]]

export HOOK_TEST_RESULT=change
expect_rejected origin HEAD:refs/heads/changed
git restore tracked.txt

export HOOK_TEST_RESULT=pass
echo second >> tracked.txt
git add tracked.txt
git commit --quiet -m "Second fixture"
expect_rejected origin "$first:refs/heads/old-commit"
git push --quiet origin HEAD:refs/heads/test
git -c core.hooksPath=/dev/null tag -a tested-tag -m "Annotated tag"
git push --quiet origin tested-tag

# Deleting a remote ref must not run application checks.
export HOOK_TEST_RESULT=fail
git push --quiet origin --delete test
echo "Pre-push integration checks passed."
