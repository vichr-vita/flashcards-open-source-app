import { stdin, stdout } from "node:process";
import { closeDatabase, query } from "../db.js";
import { getLocalAuthConfig } from "./config.js";
import { bootstrapAccount, issueEnrollment, revokePasskey, revokeAllSessions } from "./admin.js";

function printEnrollment(url: string): void {
  stdout.write(`Open this private, single-use link within 10 minutes:\n${url}\nKeep it out of shared logs. Save the passkey, then sign in.\n`);
}

async function main(): Promise<void> {
  if (process.env.AUTH_MODE !== "local") throw new Error("Administrative commands require AUTH_MODE=local");
  getLocalAuthConfig();
  if (!stdin.isTTY || !stdout.isTTY) throw new Error("Use an interactive SSH terminal; do not redirect enrollment links into logs");
  switch (process.argv[2]) {
    case "bootstrap": {
      const account = await bootstrapAccount();
      stdout.write(`Account created: ${account.userId}\n`);
      printEnrollment(account.enrollmentUrl);
      break;
    }
    case "add-passkey": printEnrollment(await issueEnrollment(false)); break;
    case "reset-passkeys": printEnrollment(await issueEnrollment(true)); stdout.write("Old passkeys and all sessions revoked.\n"); break;
    case "revoke-passkey": {
      if (!process.argv[3]) throw new Error("Pass a credential ID from status");
      await revokePasskey(process.argv[3]); stdout.write("Passkey and all sessions revoked.\n"); break;
    }
    case "revoke-sessions": await revokeAllSessions(); stdout.write("All sessions revoked.\n"); break;
    case "status": {
      const result = await query<{ user_id: string; sessions: string }>("SELECT user_id, (SELECT count(*) FROM auth.local_sessions WHERE refresh_expires_at > now()) AS sessions FROM auth.local_account", []);
      if (!result.rows[0]) { stdout.write("No local account.\n"); break; }
      stdout.write(`Account: ${result.rows[0].user_id}\nActive sessions: ${result.rows[0].sessions}\n`);
      const keys = await query<{ credential_id: string; created_at: Date; last_used_at: Date | null }>("SELECT credential_id, created_at, last_used_at FROM auth.local_passkeys ORDER BY created_at", []);
      for (const key of keys.rows) stdout.write(`Passkey: ${key.credential_id}\nCreated: ${key.created_at.toISOString()}\nLast used: ${key.last_used_at?.toISOString() ?? "never"}\n`);
      break;
    }
    default: throw new Error("Usage: local-account <bootstrap|add-passkey|reset-passkeys|revoke-passkey ID|revoke-sessions|status>");
  }
}

try { await main(); }
catch (error) { console.error(error instanceof Error ? error.message : "Account command failed"); process.exitCode = 1; }
finally { await closeDatabase(); }
