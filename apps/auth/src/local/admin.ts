import { randomUUID } from "node:crypto";
import { Secret } from "otpauth";
import { applyUserDatabaseScopeInExecutor, transaction } from "../db.js";
import { ensureUserSettingsAndSelectWorkspace } from "../server/agent/userWorkspace.js";
import { createAuthenticator, encryptSecret, hashPassword } from "./credentials.js";

function enrollmentCounter(secret: Secret, code: string): number {
  const timestamp = Date.now();
  const delta = /^\d{6}$/.test(code) ? createAuthenticator(secret).validate({ token: code, timestamp, window: 1 }) : null;
  if (delta === null) throw new Error("Authenticator code is incorrect; account was not changed");
  return Math.floor(timestamp / 30000) + delta;
}

/** Owner-only provisioning creates the profile, workspace, and confirmed MFA credentials atomically. */
export async function bootstrapAccount(password: string, secret: Secret, code: string): Promise<string> {
  const counter = enrollmentCounter(secret, code);
  const passwordHash = await hashPassword(password);
  return transaction(async executor => {
    await executor.query("SELECT pg_advisory_xact_lock(165, 1)", []);
    const existing = await executor.query("SELECT 1 FROM auth.local_account", []);
    if (existing.rows.length) throw new Error("Local account already exists; use a reset command");
    const userId = randomUUID();
    await applyUserDatabaseScopeInExecutor(executor, { userId });
    const workspaceId = await ensureUserSettingsAndSelectWorkspace(executor, userId, null);
    await executor.query("UPDATE org.user_settings SET workspace_id = $1 WHERE user_id = $2", [workspaceId, userId]);
    await executor.query(
      "INSERT INTO auth.local_account (user_id, password_hash, totp_secret_encrypted, last_totp_counter) VALUES ($1, $2, $3, $4)",
      [userId, passwordHash, encryptSecret(secret.base32), counter],
    );
    return userId;
  });
}

/** Resets preserve application identity, serialize with login, and invalidate every session. */
export async function resetCredentials(input: { password?: string; secret?: Secret; code?: string }): Promise<void> {
  if (input.password === undefined && input.secret === undefined) throw new Error("A credential is required");
  const passwordHash = input.password === undefined ? null : await hashPassword(input.password);
  const counter = input.secret === undefined ? null : enrollmentCounter(input.secret, input.code ?? "");
  const secretEncrypted = input.secret === undefined ? null : encryptSecret(input.secret.base32);
  await transaction(async executor => {
    const result = await executor.query<{ user_id: string }>("SELECT user_id FROM auth.local_account WHERE singleton FOR UPDATE", []);
    if (!result.rows[0]) throw new Error("Local account does not exist; reset cannot recreate it");
    await executor.query(
      "UPDATE auth.local_account SET password_hash = COALESCE($1, password_hash), totp_secret_encrypted = COALESCE($2, totp_secret_encrypted), last_totp_counter = COALESCE($3, last_totp_counter), failed_attempts = 0, locked_until = NULL WHERE singleton",
      [passwordHash, secretEncrypted, counter],
    );
    await executor.query("DELETE FROM auth.local_sessions", []);
  });
}

export async function revokeAllSessions(): Promise<void> {
  await transaction(async executor => {
    await executor.query("SELECT user_id FROM auth.local_account WHERE singleton FOR UPDATE", []);
    await executor.query("DELETE FROM auth.local_sessions", []);
  });
}
