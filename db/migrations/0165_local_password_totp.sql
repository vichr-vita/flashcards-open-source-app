-- Schemas touched/read explicitly: auth, org, pg_catalog.
-- Optional single-account authentication for browser-only self-hosting.
-- Credentials can only be provisioned by the database owner, never the runtime role.
CREATE TABLE auth.local_account (
  singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
  user_id TEXT NOT NULL UNIQUE REFERENCES org.user_settings(user_id) ON DELETE CASCADE,
  password_hash TEXT NOT NULL,
  totp_secret_encrypted TEXT NOT NULL,
  last_totp_counter BIGINT NOT NULL DEFAULT -1,
  failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
  locked_until TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE auth.local_sessions (
  session_hash TEXT PRIMARY KEY CHECK (length(session_hash) = 64),
  refresh_hash TEXT NOT NULL UNIQUE CHECK (length(refresh_hash) = 64),
  user_id TEXT NOT NULL REFERENCES auth.local_account(user_id) ON DELETE CASCADE,
  expires_at TIMESTAMPTZ NOT NULL,
  refresh_expires_at TIMESTAMPTZ NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  CHECK (expires_at <= refresh_expires_at)
);
CREATE INDEX local_sessions_refresh_expiry ON auth.local_sessions(refresh_expires_at);

REVOKE ALL ON auth.local_account, auth.local_sessions FROM PUBLIC;
GRANT SELECT ON auth.local_account TO auth_app;
GRANT UPDATE (last_totp_counter, failed_attempts, locked_until) ON auth.local_account TO auth_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON auth.local_sessions TO auth_app;

-- The backend may verify sessions but cannot read passwords, TOTP secrets, or refresh hashes.
CREATE FUNCTION auth.verify_local_session(token_hash TEXT) RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog
AS $$
  SELECT session.user_id FROM auth.local_sessions AS session
  JOIN auth.local_account AS account ON account.user_id = session.user_id
  WHERE session.session_hash = token_hash
    AND session.expires_at > statement_timestamp()
    AND session.refresh_expires_at > statement_timestamp()
$$;
REVOKE ALL ON FUNCTION auth.verify_local_session(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth.verify_local_session(TEXT) TO backend_app, auth_app;
