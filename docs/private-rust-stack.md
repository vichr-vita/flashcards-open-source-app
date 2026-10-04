# Private Rust stack

This rewrite covers the single-account browser installation. The AWS, native,
and admin sources remain upstream reference code. The browser keeps the existing
design, IndexedDB storage, HTTP/JSON contracts, and offline synchronization.

The runtime is Axum and Tokio with SQLx PostgreSQL connections. Clap provides
the server, migration, passkey account, and owner agent-key commands. Rust types
generate the browser wire contracts through ts-rs. The web build uses React 19,
Vite, TanStack Router and Query, Effect decoders, Tailwind 4, and Radix components.
Documentation lives in `apps/docs` with Astro and MDX.

## Build and check

```sh
pnpm install --frozen-lockfile
cargo build --locked --release
pnpm generate:types
pnpm build:web
pnpm build:docs
bash scripts/check.sh
```

Read [local verification](local-checks.md) for browser fixture dependencies.
Run `pnpm --dir apps/docs dev` to read the documentation locally.

## Run the services

Use each service's existing restricted role in `DATABASE_URL`. Backend settings
include `PUBLIC_AUTH_BASE_URL`, `PUBLIC_APP_BASE_URL`, `BACKEND_ALLOWED_ORIGINS`,
`COOKIE_DOMAIN`, and the persistent `BACKEND_CSRF_SECRET`. Auth settings use the
same public auth origin and `ALLOWED_REDIRECT_URIS`. Preserve the passkey RP
hostname, cookie domain, and secrets of the installed deployment.

```sh
target/release/lingvichr serve --service backend --bind 127.0.0.1:19400
target/release/lingvichr serve --service auth --bind 127.0.0.1:19401
```

For a combined isolated preview, set `DATABASE_URL` to its backend role and
`AUTH_DATABASE_URL` to its separate auth role, then use `--service all` and
`--web-dir apps/web/dist`. The combined service serves the built browser too.
Loopback HTTP requires both `NODE_ENV=development` and
`LOCAL_AUTH_ALLOW_HTTP=true`. The RP hostname must be `localhost`.

`infra/private/Dockerfile` builds the private binary and browser. Its Compose
example binds port 19410 only on loopback, accepts explicitly supplied isolated
database URLs in `PRIVATE_BACKEND_DATABASE_URL` and `PRIVATE_AUTH_DATABASE_URL`,
and requires `BACKEND_CSRF_SECRET`. It creates no database and runs no migrations.
Its local defaults are a preview configuration. The installed Pi's Compose,
protected environment files, volume ownership, and tunnel routing still govern
a production cutover.

## Owner commands

Load the protected owner `DATABASE_URL` in an administrator terminal.

```sh
target/release/lingvichr account status
target/release/lingvichr account add-passkey
target/release/lingvichr account revoke-sessions
```

Passkey reset keeps the account UUID, user handle, workspaces, cards, and reviews.
Enrollment links and issued agent keys are credentials. Keep their output out
of shared logs. Read the [passkey guide](self-hosted-local-auth.md) before recovery.

Local MCP is optional. Enable it with `AUTH_MODE=local`,
`LOCAL_MCP_ENABLED=true`, the canonical owner UUID in `LOCAL_MCP_USER_ID`, and
an explicit `PUBLIC_APP_BASE_URL`. The endpoint is `/v1/mcp`. It accepts owner
agent keys, checks the expected host and any supplied origin, and shares the
same eight tools as chat. Browser cookies cannot authorize it.

```sh
target/release/lingvichr agent-key issue "Private client"
target/release/lingvichr agent-key list
target/release/lingvichr agent-key revoke CONNECTION_ID
```

Use `agent-key --help` for command arguments. Issuance prints a key once. Listing
and revocation never print stored secrets.

## Preserve the installed database

The migration command retains `public.schema_migrations` and full filenames,
including historical duplicate numeric prefixes. It skips installed SQL files.
The new WebAuthn library state column is nullable and additive. The server does
not run migrations at startup. It does not reset credentials or rotate roles.

Set the owner `MIGRATION_DATABASE_URL` for a reviewed migration run. Use the Pi's
existing role names through `--backend-role`, `--auth-role`, and
`--reporting-role`. Read the Astro migration page before cutover. First validate
a restored backup in an isolated PostgreSQL 16 database. Keep the existing
volume, media registry, ChatGPT credential directory, images, and release
metadata available throughout a separately authorized cutover. The ChatGPT
directory must retain its service UID, 0700 directory permissions, and 0600
credential file permissions. Set the container build's `APP_UID` and `APP_GID`
to that existing identity before mounting it.

This private deployment currently has no S3 media provider, email delivery, or
OpenAI API key. The rewrite preserves those unavailable-provider errors. Existing
logical media metadata remains in PostgreSQL; adding object storage is a
separate task.
