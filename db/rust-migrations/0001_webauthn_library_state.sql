-- Schemas touched explicitly: auth.
-- Existing credentials, opaque sessions, enrollment grants and user handles stay byte-identical.
-- State is nullable so an old pending ceremony can expire without blocking deployment.
ALTER TABLE auth.local_webauthn_challenges
  ADD COLUMN IF NOT EXISTS library_state JSONB;
