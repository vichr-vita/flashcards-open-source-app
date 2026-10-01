import { verify } from "@node-rs/argon2";
import { query, transaction } from "../db.js";
import { createAuthenticator, decryptSecret, hashToken, newToken } from "./credentials.js";

export const refreshSeconds = 30 * 24 * 60 * 60;

type Account = {
  user_id: string;
  password_hash: string;
  totp_secret_encrypted: string;
  last_totp_counter: string;
  failed_attempts: number;
  locked_until: Date | null;
  now: Date;
};
type LoginResult =
  | { status: "valid"; sessionToken: string; refreshToken: string }
  | { status: "invalid" | "replayed" }
  | { status: "throttled"; retryAfter: number };

/** The account lock serializes verification, replay protection, throttling, and credential resets. */
export async function login(password: string, code: string): Promise<LoginResult> {
  return transaction(async executor => {
    const result = await executor.query<Account>("SELECT *, clock_timestamp() AS now FROM auth.local_account WHERE singleton FOR UPDATE", []);
    const account = result.rows[0];
    if (!account) return { status: "invalid" };
    const now = account.now.getTime();
    if (account.locked_until && account.locked_until.getTime() > now) {
      return { status: "throttled", retryAfter: Math.max(1, Math.ceil((account.locked_until.getTime() - now) / 1000)) };
    }
    const passwordValid = await verify(account.password_hash, password);
    const timestamp = Date.now();
    const delta = passwordValid
      ? createAuthenticator(decryptSecret(account.totp_secret_encrypted)).validate({ token: code, window: 1, timestamp })
      : null;
    const counter = delta === null ? null : Math.floor(timestamp / 30000) + delta;
    const replayed = counter !== null && counter <= Number(account.last_totp_counter);
    if (counter === null || replayed) {
      const failures = account.locked_until ? 1 : account.failed_attempts + 1;
      await executor.query(
        "UPDATE auth.local_account SET failed_attempts = $1, locked_until = CASE WHEN $1 >= 5 THEN clock_timestamp() + interval '60 seconds' ELSE NULL END WHERE singleton",
        [failures],
      );
      if (failures >= 5) return { status: "throttled", retryAfter: 60 };
      return { status: replayed ? "replayed" : "invalid" };
    }
    await executor.query("UPDATE auth.local_account SET last_totp_counter = $1, failed_attempts = 0, locked_until = NULL WHERE singleton", [counter]);
    await executor.query("DELETE FROM auth.local_sessions WHERE refresh_expires_at <= clock_timestamp()", []);
    const sessionToken = newToken();
    const refreshToken = newToken();
    await executor.query(
      "INSERT INTO auth.local_sessions (session_hash, refresh_hash, user_id, expires_at, refresh_expires_at) VALUES ($1, $2, $3, clock_timestamp() + interval '15 minutes', clock_timestamp() + interval '30 days')",
      [hashToken(sessionToken), hashToken(refreshToken), account.user_id],
    );
    return { status: "valid", sessionToken, refreshToken };
  });
}

export async function verifySession(token: string): Promise<string | null> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(token)) return null;
  const result = await query<{ user_id: string | null }>("SELECT auth.verify_local_session($1) AS user_id", [hashToken(token)]);
  return result.rows[0]?.user_id ?? null;
}

/** Renew the server-side lifetime without racing other tabs' cookies or CSRF state. Both credentials are required. */
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
