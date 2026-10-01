#!/usr/bin/env node
// Run with the owner DATABASE_URL. Redirect create output into a protected file.
const { unsafeQuery } = require("../dist/database/unsafe.js");
const { createAgentApiKeyForUser, normalizeAgentApiKeyLabel, revokeAgentApiKeyConnectionForUser, listAgentApiKeyConnectionsPageForUser } = require("../dist/agent/apiKeys.js");

async function main() {
  if (process.env.AUTH_MODE !== "local") throw new Error("This command requires AUTH_MODE=local");
  // Runtime roles cannot read local_account, so they cannot issue keys through this command.
  const account = await unsafeQuery("SELECT user_id FROM auth.local_account WHERE singleton", []);
  const userId = account.rows[0]?.user_id;
  if (!userId) throw new Error("Local account does not exist");
  switch (process.argv[2]) {
    case "create": {
      const key = await createAgentApiKeyForUser(userId, normalizeAgentApiKeyLabel(process.argv[3] ?? "OpenCode Nibomo"));
      return { userId, ...key };
    }
    case "list": return listAgentApiKeyConnectionsPageForUser(userId, { limit: 100, cursor: null });
    case "revoke": {
      if (!process.argv[3]) throw new Error("Pass a connection ID from list");
      return revokeAgentApiKeyConnectionForUser(userId, process.argv[3]);
    }
    default: throw new Error("Usage: local-mcp-key.cjs <create [label]|list|revoke CONNECTION_ID>");
  }
}

main().then(
  result => process.stdout.write(JSON.stringify(result) + "\n", () => process.exit(0)),
  error => { console.error(error instanceof Error ? error.message : "MCP key command failed"); process.exit(1); },
);
