# Local verification

Run `bash scripts/check.sh` before pushing this private fork. It uses the same
Rust, PostgreSQL, WebAuthn, and browser flows as the Private Rust stack workflow.

## Setup

Install Docker, Rust through rustup, Node 24, pnpm 12.5.1, and Python 3. The Rust
toolchain and Clippy configuration match `~/projects/workout`.

```sh
pnpm install --frozen-lockfile
npm ci --prefix apps/auth
npm ci --prefix apps/backend
pnpm --dir apps/web exec playwright install --with-deps chromium webkit
bash scripts/check.sh
```

The two npm installs support the existing compatibility fixtures. The private
browser and documentation dependencies use the root pnpm lockfile. The private
runtime is Rust and does not launch either Node service.

If Node 24 is not active and mise is installed, the check command uses mise's
Node 24.21.0 without changing the global runtime.

## What runs

The command checks formatting and strict Clippy, builds the Rust binary, runs
the migration command twice, and tests real HTTP and PostgreSQL contracts under
the restricted runtime roles. It checks generated TypeScript types, builds the
browser and Astro/MDX docs, then exercises passkey account management, chat,
offline sync, and browser review flows. Playwright covers Chromium and narrow
WebKit layouts with populated screens and open dialogs.

Each run starts a fresh PostgreSQL 16 container on loopback port 29432. It first
applies the installed SQL history, then tests the Rust runner against the same
filename ledger. An occupied port fails startup. The script never connects to
an existing database and removes its own container on exit. Inherited production
database URLs do not select the fixture database.

The HTTP fixture also needs ports 19400, 19401, 19402, 19411, and 4318. Its
software authenticator and ChatGPT provider contain throwaway credentials.
These checks contact no production services and run no AWS deployment commands.

## Push hook

```sh
bash scripts/install-hooks.sh
```

The installer sets this repository's local `core.hooksPath`. Linked worktrees
share it, so keep the checkout containing the hook available. The hook checks
the caller's checkout, refuses dirty work or an outgoing revision other than
HEAD, and verifies it again after the checks. It never stashes or resets files.

`bash scripts/test-hooks.sh` exercises push behavior against disposable Git
repositories. `git push --no-verify` deliberately bypasses the hook.
