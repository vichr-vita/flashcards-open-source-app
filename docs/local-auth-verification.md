# Local auth implementation verification

The implementation starts at `cafb497df760e0b4aae2f5ea4d553726f2fe0dac` on branch `feat/local-password-totp`. Verification used a disposable PostgreSQL 16 container and loopback HTTP services with generated test credentials. The VPS, its database, and Vikunja were untouched.

## Completed checks

- Auth, backend, and web builds pass with Node 24.21.0.
- The full migration chain applies to an empty PostgreSQL database, including `0165_local_password_totp.sql`.
- The auth Docker image builds independently. Its Argon2 implementation hashes and verifies a throwaway password inside the Alpine runtime image.
- The real HTTP/PostgreSQL integration passes password and TOTP rejection, missing-factor rejection, replay rejection, concurrent code consumption, restart-persistent throttling, login CSRF, backend CSRF, stable account/workspace identity, card creation, review scheduling, cross-session sync, and persistence after backend restart.
- Session checks pass server-side access expiry, refresh with both cookies, concurrent refresh with stable CSRF, rejection with only a refresh cookie, absolute expiry, logout, and administrative reset/revocation.
- Reset checks preserve identity, revoke existing sessions, reject old credentials, and refuse to recreate a deleted account. Role checks confirm that backend cannot read local credentials/sessions and auth runtime cannot change password hashes.
- Local-mode probes reject bearer/guest/API-key authentication and find no mounted email OTP, native refresh, OAuth issuance, agent, or admin login path. A preload blocks outbound HTTP in the server processes and records any attempt; the flow makes no such attempt.
- Existing auth tests pass, 39 checks. Targeted existing backend tests pass, 13 checks. Existing web session/recovery and media-upload lifecycle tests pass, 22 checks. No new unit tests were added.
- Repository static checks and whitespace checks pass.

## Actual browser exercise

The collaborative desktop browser used a 390 by 844 CSS-pixel viewport for the phone layout. The selected A login has a black background, both factor fields, password autofill, one-time-code autofill, and no horizontal overflow. Password plus a generated test TOTP establishes a real session and opens the workspace.

In the web UI, a card was created online and reached PostgreSQL. All three local HTTP services were then stopped. Reload used the cached public shell and IndexedDB workspace. A second card was created offline and remained absent from PostgreSQL until services resumed. An offline review also remained local until reconnect; after session verification, its review and schedule reached PostgreSQL. Account/workspace IDs stayed unchanged through these restarts.

The browser cache contained only the public build artifacts, with no auth/API requests. Offline bootstrap now retains cached local state on a network failure, loads cached scheduler settings, and retries authentication on reconnect/focus or its background interval. Remote sync and uploads continue to require a verified session.

## Pending checks

Real Safari and an installed iPhone Home Screen app were unavailable. Home Screen installation, its session-storage behavior, offline launch/resume, and cross-device sync need the [manual phone checklist](self-hosted-local-auth.md#pending-real-iphone-check) after an authorized deployment. Desktop viewport emulation does not establish those results.

The real deployment has not been migrated, configured, bootstrapped, or restarted. Credential enrollment through the interactive SSH CLI and the deployed HTTPS cookie/redirect configuration must be checked there only after authorization. The integration already exercises the same enrollment-confirmation and reset transactions with throwaway credentials.

Existing web build warnings about its CommonJS Vite configuration and large HEIC conversion chunk remain. AI/media integrations and native clients were outside these checks.
