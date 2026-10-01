import { randomUUID } from "node:crypto";
import { applyUserDatabaseScopeInExecutor, transaction, type DatabaseExecutor } from "../db.js";
import { ensureUserSettingsAndSelectWorkspace } from "../server/agent/userWorkspace.js";
import { hashToken, newToken } from "./credentials.js";
import { getLocalAuthConfig } from "./config.js";

async function lockAccount(executor: DatabaseExecutor): Promise<string> {
  const result = await executor.query<{ user_id: string }>("SELECT user_id FROM auth.local_account WHERE singleton FOR UPDATE", []);
  if (!result.rows[0]) throw new Error("Local account does not exist; recovery cannot recreate it");
  return result.rows[0].user_id;
}

async function issueGrant(executor: DatabaseExecutor, userId: string): Promise<string> {
  const token = newToken();
  await executor.query("DELETE FROM auth.local_enrollment_grants", []);
  await executor.query("INSERT INTO auth.local_enrollment_grants (grant_hash, user_id, expires_at) VALUES ($1, $2, clock_timestamp() + interval '10 minutes')", [hashToken(token), userId]);
  const url = new URL("/enroll", getLocalAuthConfig().authOrigin);
  url.hash = `enroll=${token}`;
  return url.toString();
}

/** Owner-only bootstrap. Enrollment grants never establish an authenticated session. */
export async function bootstrapAccount(): Promise<{ userId: string; enrollmentUrl: string }> {
  return transaction(async executor => {
    await executor.query("SELECT pg_advisory_xact_lock(165, 1)", []);
    if ((await executor.query("SELECT 1 FROM auth.local_account", [])).rows.length) throw new Error("Local account already exists; use add-passkey or reset-passkeys");
    const userId = randomUUID();
    await applyUserDatabaseScopeInExecutor(executor, { userId });
    const workspaceId = await ensureUserSettingsAndSelectWorkspace(executor, userId, null);
    await executor.query("UPDATE org.user_settings SET workspace_id = $1 WHERE user_id = $2", [workspaceId, userId]);
    await executor.query("INSERT INTO auth.local_account (user_id, webauthn_user_handle) VALUES ($1, $2)", [userId, Buffer.from(userId).toString("base64url")]);
    return { userId, enrollmentUrl: await issueGrant(executor, userId) };
  });
}

/** Recovery preserves identity and revokes all old credentials, grants, challenges, and sessions. */
export async function issueEnrollment(reset: boolean): Promise<string> {
  return transaction(async executor => {
    const userId = await lockAccount(executor);
    if (reset) {
      await executor.query("DELETE FROM auth.local_passkeys", []);
      await executor.query("DELETE FROM auth.local_sessions", []);
      await executor.query("DELETE FROM auth.local_webauthn_challenges", []);
      await executor.query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL WHERE singleton", []);
    }
    return issueGrant(executor, userId);
  });
}

export async function revokePasskey(credentialId: string): Promise<void> {
  await transaction(async executor => {
    await lockAccount(executor);
    const result = await executor.query("DELETE FROM auth.local_passkeys WHERE credential_id = $1 RETURNING credential_id", [credentialId]);
    if (!result.rows.length) throw new Error("Passkey does not exist");
    await executor.query("DELETE FROM auth.local_sessions", []);
    await executor.query("DELETE FROM auth.local_webauthn_challenges", []);
    await executor.query("DELETE FROM auth.local_enrollment_grants", []);
  });
}

export async function revokeAllSessions(): Promise<void> {
  await transaction(async executor => {
    await lockAccount(executor);
    await executor.query("DELETE FROM auth.local_sessions", []);
    await executor.query("DELETE FROM auth.local_webauthn_challenges", []);
  });
}
