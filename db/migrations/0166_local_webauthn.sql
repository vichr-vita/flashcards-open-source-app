-- Schemas touched/read explicitly: auth, org, pg_catalog.
-- Replace the un-deployed password/TOTP provider while preserving application identity.
DELETE FROM auth.local_sessions;
ALTER TABLE auth.local_account ADD COLUMN webauthn_user_handle TEXT;
UPDATE auth.local_account SET webauthn_user_handle = rtrim(translate(encode(convert_to(user_id, 'UTF8'), 'base64'), '+/', '-_'), '=');
ALTER TABLE auth.local_account ALTER COLUMN webauthn_user_handle SET NOT NULL;
ALTER TABLE auth.local_account
  DROP COLUMN password_hash,
  DROP COLUMN totp_secret_encrypted,
  DROP COLUMN last_totp_counter;
UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL;

CREATE TABLE auth.local_passkeys (
  credential_id TEXT PRIMARY KEY CHECK (length(credential_id) BETWEEN 1 AND 1400),
  user_id TEXT NOT NULL REFERENCES auth.local_account(user_id) ON DELETE CASCADE,
  public_key BYTEA NOT NULL CHECK (octet_length(public_key) BETWEEN 1 AND 4096),
  counter BIGINT NOT NULL CHECK (counter >= 0),
  transports TEXT[] NOT NULL DEFAULT '{}',
  device_type TEXT NOT NULL CHECK (device_type IN ('singleDevice', 'multiDevice')),
  backed_up BOOLEAN NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at TIMESTAMPTZ
);

CREATE TABLE auth.local_enrollment_grants (
  grant_hash TEXT PRIMARY KEY CHECK (length(grant_hash) = 64),
  user_id TEXT NOT NULL REFERENCES auth.local_account(user_id) ON DELETE CASCADE,
  expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE auth.local_webauthn_challenges (
  challenge_hash TEXT PRIMARY KEY CHECK (length(challenge_hash) = 64),
  user_id TEXT NOT NULL REFERENCES auth.local_account(user_id) ON DELETE CASCADE,
  ceremony TEXT NOT NULL CHECK (ceremony IN ('registration', 'authentication')),
  browser_hash TEXT NOT NULL CHECK (length(browser_hash) = 64),
  grant_hash TEXT REFERENCES auth.local_enrollment_grants(grant_hash) ON DELETE CASCADE,
  expires_at TIMESTAMPTZ NOT NULL,
  CHECK ((ceremony = 'registration') = (grant_hash IS NOT NULL))
);

REVOKE ALL ON auth.local_passkeys, auth.local_enrollment_grants, auth.local_webauthn_challenges FROM PUBLIC;
GRANT SELECT, INSERT ON auth.local_passkeys TO auth_app;
GRANT UPDATE (counter, backed_up, last_used_at) ON auth.local_passkeys TO auth_app;
GRANT SELECT, DELETE ON auth.local_enrollment_grants TO auth_app;
GRANT SELECT, INSERT, DELETE ON auth.local_webauthn_challenges TO auth_app;
-- Enrollment grants and credential deletion remain owner-only administrative operations.
