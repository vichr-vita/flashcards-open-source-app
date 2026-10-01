# Replace password and TOTP with WebAuthn

The requested authentication method is now WebAuthn. This plan supersedes the password/TOTP design. The user selected option A from the [published login/enrollment mocks](https://alxnk1qvwfri.postplan.dev). The implementation uses that layout. Production remains untouched. See [verification](local-auth-verification.md) for completed and pending checks.

## Authentication and identity

Keep the pre-created singleton application account, its UUID, workspaces, cards, and existing opaque session/refresh cookies. Use a discoverable WebAuthn credential with user verification required for registration and sign-in. Do not constrain credentials to a platform authenticator, so compatible security keys and the browser's cross-device choices remain possible. Store multiple credentials against the same account. There is no username, email, password, TOTP, or guest fallback.

Use the maintained SimpleWebAuthn server and browser packages. The registry currently reports server `14.0.3` and browser `14.0.0`; pin compatible releases when implementing. The existing Node 24 runtime meets the server requirements. Bundle the browser helper into the auth image and serve it locally, with no CDN dependency or inline event handlers.

Set the relying-party ID to the exact configured auth hostname without a port. Verify the exact HTTPS auth origin including its port. Make those values explicit and stable. Reject unsupported origin/RP configurations before serving. Keep localhost as the isolated development exception. Changing the production hostname requires enrolling new credentials; it must not silently broaden the expected origin or RP ID.

Login uses browser-bound, short-lived challenges stored in PostgreSQL, with distinct registration/authentication types, account binding, and expiry. Consume each challenge once under a transaction lock, including rejected completed attempts. Verify challenge, origin, RP ID, credential owner, signature, user presence, and user verification before creating a session. Preserve login CSRF, trusted-origin checks, request size limits, and throttling. Use library counter handling, including supported zero-counter/synced passkeys, rather than treating every zero counter as a replay. One-use challenges remain mandatory regardless of counter behavior.

## Bootstrap, enrollment, and recovery

SSH bootstrap creates the account and issues a random enrollment grant with a short expiry. Only its hash is stored. A browser must present that grant before receiving registration options. It binds enrollment to the singleton account and a browser challenge. Atomically consume it when committing the verified credential. Do not issue a session merely for opening an enrollment link or completing credential registration; establish the session through a separate signed authentication ceremony.

Prefer a URL fragment for the enrollment grant so it does not enter normal HTTP access logs or referrers. The protected enrollment page reads it, removes it from the address/history, and passes it only in same-origin requests. Serve enrollment responses with no-store, restrictive CSP, and no third-party resources. Give expired/used grants a clear error and require another administrator-issued link. There is no open registration or public account creation.

Administrator commands can add a passkey, list credential IDs and creation dates, revoke a credential, revoke all sessions, or reset all passkeys and issue a fresh enrollment grant. Revocation/reset invalidates relevant challenges and sessions. Recovery preserves the application UUID and data. Reset never recreates a missing account. A user without any usable credential recovers through SSH, with no password/OTP downgrade.

## Migration and changes

Keep `0165_local_password_totp.sql` immutable. Add a forward migration for public-key credentials, challenges, and enrollment grants. Remove obsolete password/TOTP columns and credential-specific grants only in that new migration. Preserve the singleton account and session verifier contract. Invalidate old password/TOTP sessions during conversion so future authenticated sessions come from verified WebAuthn assertions.

Concentrate changes in `apps/auth/src/local`, its administrative CLI, auth dependencies/build, and the new migration. The backend continues to verify server-side sessions and require an existing profile. Preserve its CSRF checks, browser-only credential restriction, and disabled alternate issuance paths. Replace the password/TOTP integration fixture with a WebAuthn ceremony that uses actual signatures and the real HTTP/database services.

Ensure the existing PWA worker excludes the enrollment page and every WebAuthn auth endpoint. Keep public app-shell caching and IndexedDB sync behavior. Do not edit native clients, AWS deployment workflows, or live configuration.

## Verification

Use an isolated PostgreSQL database and real auth/backend HTTP services. Verify bootstrap grants, enrollment, multiple credentials on one stable identity, successful signed login, incorrect signature, wrong challenge/origin/RP, missing user verification, expired grants/challenges, concurrent replay, revoked credentials, reset, throttling, CSRF, session expiry/refresh/logout, restart persistence, and real card/review sync. Confirm password and TOTP endpoints cannot establish a session, and the auth flow makes no AWS/email calls.

Exercise the selected browser UI and retain manual hardware tests where browser tooling cannot provide a virtual authenticator. Real iPhone Safari and installed Home Screen app checks remain pending until a separately authorized deployment. Verify enrollment/sign-in, cross-device behavior, persistence, offline use/resumed sync, and SSH recovery there. Do not infer iPhone results from a desktop viewport.

## References

- [SimpleWebAuthn server](https://simplewebauthn.dev/docs/packages/server) describes credential storage, RP/origin validation, and verification methods.
- [SimpleWebAuthn passkeys](https://simplewebauthn.dev/docs/advanced/passkeys) describes discoverable credentials and enforcing user verification.
- [SimpleWebAuthn browser](https://simplewebauthn.dev/docs/packages/browser) describes registration/authentication and browser support checks.
- [Apple passkey browser support](https://developer.apple.com/documentation/authenticationservices/passkey-use-in-web-browsers) describes platform credential prompts. Real iPhone behavior still requires device verification.
