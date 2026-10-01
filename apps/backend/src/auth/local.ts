import { createHash } from "node:crypto";
import { unsafeQuery } from "../database/unsafe";

/** Database lookup makes expiry, logout, and administrator revocation effective on every request. */
export async function verifyLocalSession(token: string): Promise<string | null> {
  if (!/^[A-Za-z0-9_-]{43}$/.test(token)) return null;
  const result = await unsafeQuery<{ user_id: string | null }>(
    "SELECT auth.verify_local_session($1) AS user_id",
    [createHash("sha256").update(token).digest("hex")],
  );
  return result.rows[0]?.user_id ?? null;
}

export function getLocalCsrfSecret(): string {
  const secret = process.env.BACKEND_CSRF_SECRET ?? "";
  if (Buffer.byteLength(secret) < 32) throw new Error("Local auth requires BACKEND_CSRF_SECRET of at least 32 bytes");
  return secret;
}
