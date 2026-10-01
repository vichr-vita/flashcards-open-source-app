import { query, type DatabaseExecutor } from "../db.js";
import { hashToken, newToken } from "./credentials.js";

export const refreshSeconds = 30 * 24 * 60 * 60;

/** Called only after a verified WebAuthn assertion, inside its transaction. */
export async function createSession(executor: DatabaseExecutor, userId: string) {
  const sessionToken = newToken();
  const refreshToken = newToken();
  await executor.query("DELETE FROM auth.local_sessions WHERE refresh_expires_at <= clock_timestamp()", []);
  await executor.query(
    "INSERT INTO auth.local_sessions (session_hash, refresh_hash, user_id, expires_at, refresh_expires_at) VALUES ($1, $2, $3, clock_timestamp() + interval '15 minutes', clock_timestamp() + interval '30 days')",
    [hashToken(sessionToken), hashToken(refreshToken), userId],
  );
  return { sessionToken, refreshToken };
}

export async function verifySession(token: string): Promise<string | null> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(token)) return null;
  const result = await query<{ user_id: string | null }>("SELECT auth.verify_local_session($1) AS user_id", [hashToken(token)]);
  return result.rows[0]?.user_id ?? null;
}

/** Both cookies are required. Stable tokens avoid concurrent-tab cookie and CSRF races. */
export async function refresh(token: string, sessionToken: string): Promise<string | null> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(token) || !/^[A-Za-z0-9_-]{43}$/.test(sessionToken)) return null;
  const result = await query<{ session_hash: string }>(
    "UPDATE auth.local_sessions SET expires_at = LEAST(clock_timestamp() + interval '15 minutes', refresh_expires_at) WHERE session_hash = $1 AND refresh_hash = $2 AND refresh_expires_at > clock_timestamp() RETURNING session_hash",
    [hashToken(sessionToken), hashToken(token)],
  );
  return result.rows.length === 1 ? sessionToken : null;
}

export async function revoke(sessionToken: string, refreshToken: string): Promise<void> {
  await query("DELETE FROM auth.local_sessions WHERE session_hash = $1 OR refresh_hash = $2", [hashToken(sessionToken), hashToken(refreshToken)]);
}
