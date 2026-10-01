# Self-hosted password and authenticator login

This fork supports one pre-created account in desktop browsers and the mobile web app. Set `AUTH_MODE=local` in both services. Every new session requires a password and a six-digit authenticator code. There is no registration, email delivery, email recovery, or public enrollment endpoint.

The application keeps its existing workspace, card, review, and sync model. Bootstrap creates one stable application UUID and a workspace. Credential resets preserve that identity and its data. Local identities have no Cognito binding and no email address.

This guide describes the fork. The installation runbook remains the authority for the actual host, private URLs, Compose services, database role mappings, migrations, backups, and restore process. No live deployment has been performed for this change.

## Configure the services

Use Node 24.21.0 and PostgreSQL 16. Build `apps/auth`, `apps/backend`, and `apps/web` with their existing `npm ci` and `npm run build` commands. The auth Dockerfile also builds independently from `apps/auth`.

Keep secret environment files outside the checkout, readable only by the administrator. Use different database URLs for the auth runtime, backend runtime, and administrative commands. The administrative URL must use the database owner; neither runtime role can bootstrap or change credentials.

| Setting | Where | Value |
| --- | --- | --- |
| `AUTH_MODE` | Auth and backend | `local` |
| `DATABASE_URL` | Each service | Its existing isolated database and runtime role |
| `LOCAL_AUTH_ENCRYPTION_KEY` | Auth and administrative commands | One persistent base64-encoded random 32-byte key |
| `BACKEND_CSRF_SECRET` | Backend | A separate persistent random secret of at least 32 bytes |
| `PUBLIC_AUTH_BASE_URL` | Auth and backend | Exact HTTPS auth origin, including its port |
| `PUBLIC_APP_BASE_URL` | Backend | Exact HTTPS web origin |
| `ALLOWED_REDIRECT_URIS` | Auth | Comma-separated exact web origins, without paths |
| `BACKEND_ALLOWED_ORIGINS` | Backend | Comma-separated exact web origins |
| `COOKIE_DOMAIN` | Auth and backend | Shared hostname or parent domain, without scheme or port |
| `VITE_API_BASE_URL` | Web build | HTTPS backend base URL ending in `/v1` |
| `VITE_AUTH_BASE_URL` | Web build | HTTPS auth origin |
| `VITE_APP_BASE_URL` | Web build | HTTPS web origin |

Generate each secret separately with `node -e 'console.log(require("node:crypto").randomBytes(32).toString("base64"))'` in an administrator terminal. Save it directly into the appropriate protected environment file. Do not commit it or put it in command arguments, shared logs, or published artifacts. Back up the encryption key separately from the database. Losing it requires enrolling a new authenticator over SSH.

Unset `DB_SECRET_ARN`, Cognito settings, Cognito CSRF-secret ARN settings, and demo-account credentials. Local authentication does not call AWS or an email service. Existing upstream dependencies remain for the Cognito mode. Unconfigured AI, object storage, billing, and other unrelated integrations do not become available through this auth change.

Use HTTPS and keep the existing private Tailscale access. Web, API, and auth must share a cookie domain and be on the same browser site. Separate ports on the same hostname work. An auth origin on an unrelated site does not work with this cookie contract.

Plain HTTP is allowed only for explicit loopback development with both `NODE_ENV=development` and `LOCAL_AUTH_ALLOW_HTTP=true`. Do not use these settings on a remote host. Do not use `AUTH_MODE=none`.

## Bootstrap and enroll over SSH

Apply migration `0165_local_password_totp.sql` through the installation's existing migration process. Preserve its database isolation and role-name remapping. The migration adds local credentials, sessions, and a limited session-verification function. Backend runtime access cannot read password hashes, encrypted authenticator secrets, or session tables.

Before serving the auth service, open an interactive administrator SSH terminal. Load a protected administrative environment file containing `AUTH_MODE=local`, the owner `DATABASE_URL`, the same encryption key, and the auth/redirect origins. If the file contains shell-compatible assignments, load it with `set -a`, `source /path/to/local-auth-admin.env`, then `set +a`. The path is illustrative; use the existing installation's secret-file conventions.

From the built `apps/auth` directory, run:

```sh
npm run local-account -- bootstrap
```

Enter a password of at least 12 characters and confirm it. The terminal does not echo it. Scan the QR code with Google Authenticator or a compatible app, then enter a current code. The command also displays a manual setup key. Treat both the QR code and key as credentials; do not redirect enrollment output into logs or share a terminal recording.

The transaction creates the account only after confirming MFA. There is no password-only session during setup. Bootstrap refuses to create a second account. Wait for the next 30-second code before signing in because enrollment consumes its confirmation code. TOTP runs locally and requires no Google account or network call to Google.

The browser login uses the selected single-page layout with both fields. Keep the server and phone clocks synchronized. Validation accepts the current 30-second interval and one neighboring interval on either side. A consumed interval cannot be used again, including in another browser. Wait for the next code when opening a second session.

## Recover credentials and manage sessions

Recovery uses administrator SSH access. There are no email recovery links or recovery codes. Use the same administrative environment and built auth directory:

```sh
npm run local-account -- status
npm run local-account -- reset-password
npm run local-account -- reset-totp
npm run local-account -- revoke-sessions
```

`status` prints the account UUID and the number of sessions within their absolute lifetime. `reset-password` prompts for a new password. `reset-totp` enrolls and confirms a new authenticator. Each reset revokes every session, clears throttling, and preserves the account, workspaces, and cards. Neither command recreates a missing account. `revoke-sessions` signs every browser out on its next server request.

Keep administrative SSH access independent of the phone. A replacement authenticator can be enrolled even if the old device is lost. To replace a lost encryption key, save a new key in the protected auth and administrator environments, stop the auth service, run `reset-totp` with that key, then start auth with the same key. Password reset alone cannot repair a lost TOTP encryption key.

Account deletion through the web API returns `409 LOCAL_ACCOUNT_ADMIN_REQUIRED`. Account lifetime is managed over SSH for this installation. Do not delete and bootstrap to recover credentials; that creates a new identity. Intentional data deletion requires a separate administrative procedure and authorization. Deleting the account profile cascades its local credentials and sessions, and login cannot reprovision it.

Sessions use random opaque HttpOnly cookies, with only SHA-256 token hashes stored in PostgreSQL. Cookies persist across browser and service restarts. Access lasts 15 minutes on the server; refresh requires both cookies and can extend access up to the session's absolute 30-day deadline. Refresh preserves the tokens to avoid concurrent-tab cookie and CSRF races. Logout revokes the current session. Password or authenticator reset revokes all sessions.

The non-HttpOnly `logged_in` cookie is an existing browser hint, not proof of authentication. `/v1/me` verifies the server session and supplies the existing CSRF token. Mutations still require a trusted origin and CSRF token. Five failed attempts lock the single account for 60 seconds; the lock and TOTP replay counter survive service restarts. The lock applies to every caller, so an attacker with network access can temporarily delay login. Keep access private as already configured.

Local mode accepts only browser session cookies. Bearer tokens, guest credentials, API keys, demo login, native email OTP, OAuth/MCP issuance, and agent/admin routes cannot bypass MFA. Do not expose separate upstream Lambda/MCP entrypoints in this deployment. Native clients are outside this fork's supported scope.

## Mobile web and offline use

The web build includes a standalone manifest, iPhone Home Screen icon, and service worker. The worker caches only public app-shell files. It excludes login, auth, API, authorization headers, arbitrary URLs, and credential-bearing requests. Card data and pending edits continue to use the existing IndexedDB sync implementation.

Open the app online, sign in, and allow the first sync and service-worker installation to finish before going offline. Reloading offline can then open the cached shell and existing local workspace. Server authentication and first login require connectivity. Offline copies already on the device remain readable without contacting the server; revocation cannot instantly erase an offline device. On reconnect, the app verifies or refreshes its session before syncing pending changes.

The cache version includes all public build contents. A new worker waits until pages using the previous worker close before activating. Close all open app tabs and the installed app when checking an update. Authentication responses use `Cache-Control: no-store` and never enter the worker cache.

### Pending real iPhone check

Desktop browser automation with an iPhone-sized viewport does not verify iOS Home Screen behavior. Run this check after an authorized deployment:

1. Enable Tailscale on the phone and open the private web URL in Safari. Sign in with both factors. Verify that a password alone and an incorrect code do not sign in.
2. Use Share, then Add to Home Screen. Open that installed app and check its icon, standalone layout, and session behavior. If Safari and the installed app use separate storage, establish a new session with a fresh code.
3. Create a card, review it, and verify the card and schedule in a desktop browser session. Wait for sync to finish.
4. Disable network connectivity, close and reopen the installed app, and verify the cached workspace opens. Create a card and record a review offline. Reconnect and verify both changes appear in the desktop browser after sync.
5. Restart only the Nibomo services through the established runbook and verify identity, cards, and the installed-app session persist. This step requires deployment authorization.
6. Revoke sessions over SSH and verify that the installed app cannot make authenticated requests after reconnecting. Sign in again with a new code. Check logout and both reset commands with a disposable account in a separate test installation before using them on the daily account.

Record failures and actual iOS version. Photo/media availability offline depends on the existing media behavior and is not established by caching the app shell.

## Run the isolated integration check

Use only a disposable database. The script is intentionally fixed to loopback port `19432`, database `flashcards`, and the standard repository role names. It refuses a pre-existing local account and generates its own throwaway credentials. Never forward that port to a live database.

From the repository root, after installing dependencies and building auth/backend:

```sh
docker run -d --name nibomo-local-auth-test \
  -p 127.0.0.1:19432:5432 \
  -e POSTGRES_USER=flashcards_owner -e POSTGRES_DB=flashcards \
  -e POSTGRES_HOST_AUTH_METHOD=trust \
  -v "$PWD:/workspace:ro" postgres:16
docker exec -e MIGRATION_DATABASE_URL=postgresql://flashcards_owner@localhost/flashcards \
  nibomo-local-auth-test bash /workspace/scripts/deploy/migrate.sh
npm --prefix apps/auth run test:local-integration
docker rm -f nibomo-local-auth-test
```

Wait for PostgreSQL to report readiness before migrating. Trust authentication above is for this disposable loopback fixture only. It is not a deployment example. The integration command starts real auth/backend HTTP processes on ports `19401` and `19400`. Those ports must be free. Run with Node 24.21.0.

The check covers factor rejection, replay, persisted throttling, login and backend CSRF, stable identity, card creation and review scheduling, cross-session sync, service restart, expiry, concurrent refresh, logout, reset/revocation, deleted-account behavior, disabled alternate authentication, and database role permissions. Its server-process preload blocks outbound HTTP calls to detect an unexpected Cognito or email dependency. It does not verify paid services or a real phone.

## Prepare deployment and rollback

Live deployment needs separate authorization. Follow the inspected installation runbook for the actual target and migration-copy process. Do not run the repository's generic Compose/migration commands against the shared production database; its role names differ.

Before deploying, review the migration remapping, build the three browser-stack artifacts, confirm protected secret files, and take a restorable backup using the installation's existing procedure. Preserve the private access boundary, unrelated Tailscale services, and database isolation. Configure both runtime modes and web build URLs together, bootstrap over SSH, then enable the auth service through the existing Compose layout. Do not run upstream AWS or native release workflows.

Check login, `/v1/me`, CSRF, refresh, logout, and card/review sync against the private URLs. Verify restart persistence and run the phone checklist. Retain the old images and configuration for rollback.

For rollback, stop the affected services, revoke local sessions with the administrative command, and restore the previous images/configuration through the runbook. The new migration is additive; leave its tables in place rather than dropping data during an image rollback. Reverting to the previous Cognito configuration also restores its previous sign-in limitation. Restore a database backup only through the runbook's separately authorized restore process.

## Library references

TOTP validation and provisioning URIs use [OTPAuth](https://github.com/hectorm/otpauth). Password hashing uses Argon2id through [node-rs Argon2](https://github.com/napi-rs/node-rs/tree/main/packages/argon2), configured with 64 MiB memory, three iterations, and one lane. Authenticator secrets use Node's AES-256-GCM with a random nonce and authentication tag. The implementation is in `apps/auth/src/local`; the backend session boundary is in `apps/backend/src/auth/local.ts`.
