# Self-hosted passkey login

This fork supports one pre-created account in desktop browsers and the mobile web app. Set `AUTH_MODE=local` in both services. Every new session requires a WebAuthn passkey with device verification. There is no password, TOTP, username, email delivery, email recovery, or public account creation. Enrollment requires a private link issued over SSH.

The application keeps its existing workspace, card, review, and sync model. Bootstrap creates one stable application UUID and a workspace. Credential resets preserve that identity and its data. Local identities have no Cognito binding and no email address.

This guide describes the fork. The installation runbook remains the authority for the actual host, private URLs, Compose services, database role mappings, migrations, backups, and restore process. No live deployment has been performed for this change.

## Configure the services

Use Node 24.21.0 and PostgreSQL 16. Build `apps/auth`, `apps/backend`, and `apps/web` with their existing `npm ci` and `npm run build` commands. The auth Dockerfile also builds independently from `apps/auth`.

Keep secret environment files outside the checkout, readable only by the administrator. Use different database URLs for the auth runtime, backend runtime, and administrative commands. The administrative URL must use the database owner; neither runtime role can bootstrap or change credentials.

| Setting | Where | Value |
| --- | --- | --- |
| `AUTH_MODE` | Auth and backend | `local` |
| `DATABASE_URL` | Each service | Its existing isolated database and runtime role |
| `WEBAUTHN_RP_ID` | Auth and administrative commands | Exact auth hostname, without scheme, path, or port |
| `BACKEND_CSRF_SECRET` | Backend | A separate persistent random secret of at least 32 bytes |
| `PUBLIC_AUTH_BASE_URL` | Auth and backend | Exact HTTPS auth origin, including its port |
| `PUBLIC_APP_BASE_URL` | Backend | Exact HTTPS web origin |
| `ALLOWED_REDIRECT_URIS` | Auth | Comma-separated exact web origins, without paths |
| `BACKEND_ALLOWED_ORIGINS` | Backend | Comma-separated exact web origins |
| `COOKIE_DOMAIN` | Auth and backend | Shared hostname or parent domain, without scheme or port |
| `VITE_API_BASE_URL` | Web build | HTTPS backend base URL ending in `/v1` |
| `VITE_AUTH_BASE_URL` | Web build | HTTPS auth origin |
| `VITE_APP_BASE_URL` | Web build | HTTPS web origin |

Generate the backend CSRF secret with `node -e 'console.log(require("node:crypto").randomBytes(32).toString("base64"))'` in an administrator terminal. Save it directly into the appropriate protected environment file. Do not commit it or put it in shared logs. WebAuthn stores public keys, not passkey private keys; there is no authenticator-secret encryption key to configure.

Set `WEBAUTHN_RP_ID` to the hostname of `PUBLIC_AUTH_BASE_URL`. The verifier checks the exact origin, including its port. Use a stable DNS hostname with HTTPS. Changing the hostname requires new passkeys; changing a port requires changing the expected origin but preserves the RP hostname. IP-address relying parties are rejected. `localhost` is the explicit development exception.

Unset `DB_SECRET_ARN`, Cognito settings, Cognito CSRF-secret ARN settings, and demo-account credentials. Local authentication does not call AWS or an email service. Existing upstream dependencies remain for the Cognito mode. Unconfigured AI, object storage, billing, and other unrelated integrations do not become available through this auth change.

For subscription-backed chat, follow [ChatGPT subscriptions on a private server](self-hosted-chatgpt.md). It adds a dedicated AI settings page and requires a private persistent credential directory.

Use HTTPS and keep the existing private Tailscale access. Web, API, and auth must share a cookie domain and be on the same browser site. Separate ports on the same hostname work. An auth origin on an unrelated site does not work with this cookie contract.

Plain HTTP is allowed only for explicit loopback development with both `NODE_ENV=development` and `LOCAL_AUTH_ALLOW_HTTP=true`. Do not use these settings on a remote host. Do not use `AUTH_MODE=none`.

## Bootstrap and enroll over SSH

Apply `0165_local_password_totp.sql` and then `0166_local_webauthn.sql` through the installation's existing migration process. Preserve database isolation and role-name remapping. Migration 0166 preserves any existing account UUID, removes password/TOTP columns, and invalidates all previous sessions. Enrollment grants, challenges, and passkey public keys are separate tables. Backend runtime access cannot read them or session tables. The auth runtime cannot issue grants, delete passkeys, or replace stored public keys.

Open an interactive administrator SSH terminal. Load a protected administrative environment file containing `AUTH_MODE=local`, the owner `DATABASE_URL`, `WEBAUTHN_RP_ID`, and the auth/redirect origins. If it contains shell-compatible assignments, load it with `set -a`, `source /path/to/local-auth-admin.env`, then `set +a`. Use the installation's existing secret-file conventions.

From the built `apps/auth` directory, run:

```sh
npm run local-account -- bootstrap
```

The command creates the stable account and workspace, then prints a private, single-use enrollment link valid for ten minutes. Open it in a browser that can reach the private auth hostname and select **Create passkey**. Confirm with your device's biometric prompt, device PIN, or a compatible security key with user verification. The browser controls which authenticator choices appear. You do not need to create a Google account or configure an email address.

The grant is in the URL fragment so normal HTTP access logs do not receive it. The page removes the fragment before making any requests and keeps it in memory. Treat the full link as a credential. Do not record, publish, redirect, or paste enrollment output into shared logs. Reloading that page requires reopening the original link while it remains unused and unexpired.

Opening the link and saving a passkey never establishes a session. Select **Sign in with passkey** after setup for a separate signed authentication. Bootstrap refuses a second account. For an account converted from migration 0165, use `add-passkey` instead of bootstrap.

## Recover passkeys and manage sessions

Use the same administrative environment and interactive SSH terminal:

```sh
npm run local-account -- status
npm run local-account -- add-passkey
npm run local-account -- revoke-passkey CREDENTIAL_ID
npm run local-account -- revoke-sessions
npm run local-account -- reset-passkeys
```

`status` prints the account UUID, session count, and public credential IDs with creation and last-use dates. `add-passkey` issues a fresh enrollment link and preserves existing passkeys and sessions. Each new link invalidates any earlier outstanding enrollment link. Up to 16 passkeys can belong to the same account.

`revoke-passkey` removes the named public credential ID and revokes every session and outstanding challenge/grant. `revoke-sessions` revokes sessions and pending sign-in challenges while keeping passkeys. `reset-passkeys` removes every passkey, revokes sessions and outstanding challenges/grants, clears throttling, and issues a fresh enrollment link. It preserves the account UUID, workspace, and data. Recovery never recreates a missing account.

Keep administrative SSH access independent of the phone. If all passkeys are lost, use `reset-passkeys` and enroll on the replacement device. There is no password, OTP, recovery-code, or email fallback. A separately enrolled backup passkey or compatible security key can avoid an SSH reset. Revoking server sessions cannot remove a credential from a device's password manager; remove obsolete passkeys there yourself.
Account deletion through the web API returns `409 LOCAL_ACCOUNT_ADMIN_REQUIRED`. Account lifetime is managed over SSH for this installation. Do not delete and bootstrap to recover credentials; that creates a new identity. Intentional data deletion requires a separate administrative procedure and authorization. Deleting the account profile cascades its local credentials and sessions, and login cannot reprovision it.

Sessions use random opaque HttpOnly cookies, with only SHA-256 token hashes stored in PostgreSQL. Cookies persist across browser and service restarts. Access lasts 15 minutes on the server; refresh requires both cookies and can extend access up to the session's absolute 30-day deadline. Refresh preserves the tokens to avoid concurrent-tab cookie and CSRF races. Logout revokes the current session. Passkey reset or revocation revokes all sessions.

The non-HttpOnly `logged_in` cookie is an existing browser hint, not proof of authentication. `/v1/me` verifies the server session and supplies the existing CSRF token. Mutations still require a trusted origin and CSRF token. Five failed attempts lock the single account for 60 seconds; the lock, credential counters, and pending challenges survive service restarts. The lock applies to every caller, so an attacker with network access can temporarily delay login. Keep access private as already configured.

Local mode accepts only browser session cookies. Bearer tokens, guest credentials, API keys, demo login, native email OTP, OAuth/MCP issuance, and agent/admin routes cannot bypass passkey verification. Do not expose separate upstream Lambda/MCP entrypoints in this deployment. Native clients are outside this fork's supported scope.

## Mobile web and offline use

The web build includes a standalone manifest, iPhone Home Screen icon, and service worker. The worker caches only public app-shell files. It excludes login, enrollment, auth, API, authorization headers, arbitrary URLs, and credential-bearing requests. Card data and pending edits continue to use the existing IndexedDB sync implementation.

Open the app online, sign in, and allow the first sync and service-worker installation to finish before going offline. Reloading offline can then open the cached shell and existing local workspace. Server authentication and first login require connectivity. Offline copies already on the device remain readable without contacting the server; revocation cannot instantly erase an offline device. On reconnect, the app verifies or refreshes its session before syncing pending changes.

The cache version includes all public build contents. A new worker waits until pages using the previous worker close before activating. Close all open app tabs and the installed app when checking an update. Authentication responses use `Cache-Control: no-store` and never enter the worker cache.

### Pending real iPhone check

Desktop browser automation with an iPhone-sized viewport does not verify iOS Home Screen behavior. Run this check after an authorized deployment:

1. Enable Tailscale on the phone and open a private enrollment link in Safari. Save the passkey and verify setup alone does not open authenticated data. Sign in with device verification. Cancel the prompt and check that no new session appears.
2. Use Share, then Add to Home Screen from the web app. Open the installed app and check its icon, standalone layout, and session behavior. If Safari and the installed app use separate storage, sign in with the passkey again.
3. Sign in from a desktop browser. Check its supported passkey choices, including cross-device sign-in if you use it. Create and review a card on the phone and verify its schedule on desktop after sync.
4. Disable network connectivity, close and reopen the installed app, and verify the cached workspace opens. Create a card and record a review offline. Reconnect and verify both changes reach the desktop browser. Initial sign-in/enrollment require connectivity.
5. After deployment authorization, restart only Nibomo through the established runbook. Verify identity, cards, and the installed-app session persist.
6. In a separate test installation, add a second passkey, revoke it, and verify all existing sessions are invalidated. Test expired/used enrollment links, logout, and SSH `reset-passkeys`. Verify old passkeys fail after reset while account/workspace IDs remain unchanged. Then repeat session revocation on the daily account only with explicit authorization.

Record failures and actual iOS version. Photo/media availability offline depends on the existing media behavior and is not established by caching the app shell.

## Run the isolated integration check

Use only a disposable database. The script is intentionally fixed to loopback port `19432`, database `flashcards`, and the standard repository role names. It refuses a pre-existing local account and generates its own throwaway software authenticator keys. Never forward that port to a live database.

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

Wait for PostgreSQL to report readiness before migrating. Trust authentication above is for this disposable loopback fixture only. It is not a deployment example. The integration command starts real auth/backend HTTP processes on ports `19401` and `19400`, plus a simulated OpenAI provider on `19402`. Those ports must be free. Run with Node 24.21.0. Optional browser review uses a separate web process on `19411`.

The check covers signed WebAuthn registration/login, missing device verification, wrong signature/origin/RP, browser binding, expiry/replay, persisted throttling, login and backend CSRF, stable identity, card creation and review scheduling, cross-session sync, service restart, expiry, concurrent refresh, logout, passkey addition/reset/revocation, deleted-account behavior, disabled alternate authentication, and database role permissions. It also exercises subscription chat through the simulated provider. Its server-process preload redirects the fixed OpenAI hosts to that fixture and blocks other outbound HTTP calls to detect an unexpected Cognito or email dependency. It does not verify paid services or a real phone.

## Prepare deployment and rollback

Live deployment needs separate authorization. Follow the inspected installation runbook for the actual target and migration-copy process. Do not run the repository's generic Compose/migration commands against the shared production database; its role names differ.

Before deploying, review both migrations and their role remapping, build the three browser-stack artifacts, confirm protected secret files, and take a restorable backup using the installation's existing procedure. Preserve the private access boundary, unrelated Tailscale services, and database isolation. Configure both runtime modes and web build URLs together, issue an enrollment link over SSH, then enable the auth service through the existing Compose layout. Do not run upstream AWS or native release workflows.

Check login, `/v1/me`, CSRF, refresh, logout, and card/review sync against the private URLs. Verify restart persistence and run the phone checklist. Retain the old images and configuration for rollback.

For rollback, stop the affected services, revoke local sessions with the administrative command, and restore the previous images/configuration through the runbook. Migration 0166 removes old credential columns. Rolling back to the password/TOTP image requires a separately authorized database restore; an image-only rollback to that implementation will not work. Preserve the new tables during any Cognito-image rollback. Reverting to the previous Cognito configuration also restores its previous sign-in limitation. Restore a database backup only through the runbook's separately authorized restore process.

## Library references

Registration and assertion verification use pinned [SimpleWebAuthn server](https://simplewebauthn.dev/docs/packages/server) `14.0.3`; the bundled [browser helper](https://simplewebauthn.dev/docs/packages/browser) is `14.0.0`. Both ceremonies require user verification. The request asks for a discoverable credential without restricting authenticator attachment. One-use, browser-bound challenges expire after two minutes, independent of whether a synced passkey uses a zero counter. The implementation is in `apps/auth/src/local`; the backend session boundary remains in `apps/backend/src/auth/local.ts`.
