import { createInterface } from "node:readline/promises";
import { stdin, stdout } from "node:process";
import QRCode from "qrcode";
import { closeDatabase, query } from "../db.js";
import { getLocalAuthConfig } from "./config.js";
import { createAuthenticator } from "./credentials.js";
import { bootstrapAccount, resetCredentials, revokeAllSessions } from "./admin.js";

/** Reads a password from the controlling terminal without echoing it or putting it in argv. */
async function readPassword(prompt: string): Promise<string> {
  stdout.write(prompt);
  stdin.setRawMode(true);
  stdin.resume();
  stdin.setEncoding("utf8");
  return new Promise((resolve, reject) => {
    let password = "";
    const done = (error?: Error) => {
      stdin.off("data", onData);
      stdin.setRawMode(false);
      stdin.pause();
      stdout.write("\n");
      if (error) reject(error); else resolve(password);
    };
    const onData = (data: string) => {
      for (const character of data) {
        if (character === "\u0003") { done(new Error("Cancelled")); return; }
        if (character === "\r" || character === "\n") { done(); return; }
        if (character === "\u007f" || character === "\b") password = [...password].slice(0, -1).join("");
        else if (character >= " ") password += character;
      }
    };
    stdin.on("data", onData);
  });
}

async function newPassword(): Promise<string> {
  const password = await readPassword("New password: ");
  if (password !== await readPassword("Confirm password: ")) throw new Error("Passwords do not match");
  return password;
}

async function enroll() {
  const authenticator = createAuthenticator();
  stdout.write("Scan this QR code with your authenticator app. Keep the setup key private.\n");
  stdout.write(await QRCode.toString(authenticator.toString(), { type: "terminal", small: true }));
  stdout.write(`Manual setup key: ${authenticator.secret.base32}\n`);
  const readline = createInterface({ input: stdin, output: stdout });
  try { return { secret: authenticator.secret, code: (await readline.question("Current 6-digit code: ")).trim() }; }
  finally { readline.close(); }
}

async function main(): Promise<void> {
  if (process.env.AUTH_MODE !== "local") throw new Error("Administrative commands require AUTH_MODE=local");
  getLocalAuthConfig();
  if (!stdin.isTTY || !stdout.isTTY) throw new Error("Use an interactive SSH terminal; do not redirect credential enrollment into logs");
  const command = process.argv[2];
  switch (command) {
    case "bootstrap": {
      const password = await newPassword();
      const { secret, code } = await enroll();
      const userId = await bootstrapAccount(password, secret, code);
      stdout.write(`Account created: ${userId}\nWait for the next authenticator code before signing in.\n`);
      break;
    }
    case "reset-password": await resetCredentials({ password: await newPassword() }); stdout.write("Password changed. All sessions revoked.\n"); break;
    case "reset-totp": await resetCredentials(await enroll()); stdout.write("Authenticator changed. All sessions revoked. Wait for the next code.\n"); break;
    case "revoke-sessions": await revokeAllSessions(); stdout.write("All sessions revoked.\n"); break;
    case "status": {
      const result = await query<{ user_id: string; sessions: string }>("SELECT user_id, (SELECT count(*) FROM auth.local_sessions WHERE refresh_expires_at > now()) AS sessions FROM auth.local_account", []);
      stdout.write(result.rows[0] ? `Account: ${result.rows[0].user_id}\nActive sessions: ${result.rows[0].sessions}\n` : "No local account.\n");
      break;
    }
    default: throw new Error("Usage: local-account <bootstrap|reset-password|reset-totp|revoke-sessions|status>");
  }
}

try { await main(); }
catch (error) { console.error(error instanceof Error ? error.message : "Account command failed"); process.exitCode = 1; }
finally { await closeDatabase(); }
