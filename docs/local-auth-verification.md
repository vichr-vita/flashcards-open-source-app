# WebAuthn verification

This fork starts at installed upstream commit `cafb497df760e0b4aae2f5ea4d553726f2fe0dac`. Branch `feat/local-webauthn` replaces the earlier local password/TOTP implementation. Checks used disposable PostgreSQL 16 databases and loopback services with generated software authenticator keys. The VPS, its database, and Vikunja were untouched.

## Completed checks

- Auth, backend, and web builds pass with Node 24.21.0. The auth build type-checks and bundles the passkey browser helper locally.
- The full migration chain applies to an empty database through `0166_local_webauthn.sql`. A separate populated 0165 database was converted to 0166: account/workspace IDs stayed unchanged, old credential columns disappeared, sessions were invalidated, and throttling cleared.
- The Alpine auth image builds independently and serves its health endpoint, selected login page, and bundled browser JavaScript with the expected content type and no-store headers.
- The real HTTP/PostgreSQL integration uses generated ES256 keys and actual signatures. It exercises the real SimpleWebAuthn verifier, not a stub. Bootstrap refuses a second account. Enrollment requires an owner-issued grant, requires device verification, consumes the grant, and never creates a session.
- Registration/login reject missing user verification, wrong origin/RP, and cross-origin client data. Login rejects bad signatures, missing user presence, wrong challenges, and a mismatched user handle. Challenges expire, belong to their requesting browser, and are consumed even on a rejected completed ceremony. Concurrent replay establishes exactly one session. Failed-attempt throttling survives auth restart.
- Multiple passkeys belong to the same account/workspace. Zero-counter synced passkeys work across new sessions. Adding/revoking/resetting passkeys preserves identity. Revocation/reset invalidates sessions and old credentials; expired and used grants fail. Recovery refuses to recreate a deleted account.
- Session checks cover access expiry, refresh with both cookies, concurrent refresh with stable CSRF, rejection with only a refresh cookie, absolute expiry, logout, and SSH session revocation.
- Backend checks cover CSRF/origin enforcement, stable identity, card creation, review scheduling, cross-session sync, and backend restart persistence. Bearer/guest/API-key, old password/email OTP, native token, OAuth, agent, and admin paths cannot establish an alternate session in local mode.
- Runtime-role checks reject backend reads of passkeys/sessions, auth replacement of public keys, auth grant creation, and auth passkey deletion. Only the owner can administer credentials/grants. Server-process preloads block outbound HTTP and record attempts; the tested flow makes no Cognito/email request.
- Existing auth tests pass, 39 checks. The integration fixture also passes strict TypeScript checking. All five repository static checks and whitespace checks pass. No new unit tests were added.

The reproducible integration command and disposable database setup are in the [self-hosting guide](self-hosted-local-auth.md#run-the-isolated-integration-check).

## Browser exercise

The collaborative desktop browser rendered option A at desktop size and a 390 by 844 CSS-pixel viewport. The background is true black, there is one passkey action, and neither layout overflows horizontally. Enrollment removes its grant from the address bar. Missing links disable setup with the selected error state. A simulated authenticator cancellation permits another attempt and displays the intended short message.

A software test authenticator supplied real attestation data and WebCrypto ES256 assertion signatures through the browser credential API. The bundled SimpleWebAuthn browser helper serialized those responses. The real auth service saved the credential without issuing cookies; a separate signed login established the session and opened the actual web workspace. `/v1/me` returned the expected stable account/workspace and session transport.

This verifies the application UI and integration, not the native biometric prompt or a hardware authenticator. The current public-shell browser cache contained 188 build files and no auth/API responses. The worker now explicitly excludes enrollment as well as login and API routes.

The earlier desktop offline exercise, before the passkey change, created a card online, stopped the local services, reloaded the cached shell, created a card and review offline, and synchronized both after reconnecting. The IndexedDB/offline sync code is unchanged by the WebAuthn replacement. That evidence does not establish real iPhone behavior.

## Pending device and deployment checks

Real Safari, Face ID/Touch ID, physical security keys, cross-device prompts, and an installed iPhone Home Screen app were unavailable. Run the [manual phone checklist](self-hosted-local-auth.md#pending-real-iphone-check) after an authorized deployment. A desktop viewport and software authenticator do not establish those results.

The actual VPS has not been migrated, configured, bootstrapped, or restarted. Its role remapping, private HTTPS origin/RP/cookie configuration, interactive enrollment, backups, and rollback must follow the inspected installation runbook after separate authorization. The local interactive CLI status check passed against the disposable fixture.

Existing web build warnings about CommonJS Vite configuration and the large HEIC conversion chunk remain. AI/media integrations and native clients are outside these checks.
