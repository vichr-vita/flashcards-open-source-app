# Local verification

Run `bash scripts/check.sh` before pushing to your fork. Install the pre-push hook to run it automatically. Hosted fork browser checks remain enabled.

## Setup

Install Node 24, npm, Python 3, and PostgreSQL server and client tools. Put `initdb`, `pg_ctl`, `createdb`, and `psql` on your PATH.

```sh
npm --prefix apps/auth ci
npm --prefix apps/backend ci
npm --prefix apps/web ci
cd apps/web
npx playwright install chromium
cd ../..
bash scripts/install-hooks.sh
bash scripts/check.sh
```

On Linux, install Playwright's Chromium system dependencies once.

The command requires Node 24. If another version is active and mise is installed, it runs through mise's Node 24.21.0 without changing your global runtime configuration.

The installer sets this clone's local `core.hooksPath` to the absolute path of its tracked `.githooks` directory. Linked worktrees share that setting, including older branches without the scripts. Keep the source checkout available. Rerun the installer after moving it or merging these scripts into your usual checkout. The hook checks the caller's checkout.

## What runs

The command runs hook integration checks, builds the disposable auth/backend/web stack, runs the real HTTP/PostgreSQL and local-account browser flows, and checks the review UI in both themes. It uses the same command as Fork browser smoke CI.

Hosted checks run on matching changes pushed to vichr-fork and on PRs targeting that branch. Feature pushes do not also run the fork workflow.

Local checks create a fresh PostgreSQL cluster on loopback port 19432, apply migrations to that isolated database, and remove it on success or failure. They ignore inherited PostgreSQL connection settings and migration credentials. An occupied port makes startup fail; the command never reuses an existing database. Run it as your ordinary user with PostgreSQL server binaries installed.

The existing fixture also uses ports 19400, 19401, 19411, and 4318. Its disposable web build goes to `/tmp/nibomo-local-web`. These checks do not use development accounts, deployed services, or AWS deployment commands.

CI uses `bash scripts/check.sh --ci` after creating and migrating its disposable PostgreSQL container.

## Push behavior

The hook rejects pushes from a dirty checkout, including untracked files, and rejects outgoing revisions other than the checked-out commit. Failed verification blocks the push. Annotated tags pointing to the checked-out commit are checked too; deletions skip verification.

It checks the checkout again afterward and never stashes or resets changes. Hooks are local and do not run when a PR is opened or merged on GitHub. `git push --no-verify` deliberately bypasses them.

Run `bash scripts/test-hooks.sh` to test push behavior against disposable local Git repositories.
