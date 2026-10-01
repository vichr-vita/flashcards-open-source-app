import { randomUUID } from "node:crypto";
import { Hono } from "hono";
import { WebStandardStreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/webStandardStreamableHttp.js";
import { authenticateAgentApiKey } from "../agent/apiKeys";
import { getAuthConfig } from "../auth/config";
import type { AppEnv } from "../server/appEnv";
import { logMcpRequestEvent } from "../server/logging";
import { HttpError } from "../shared/errors";
import { getConfiguredPublicAppOrigin } from "../shared/publicUrls";
import { createMcpServer } from "./server";
import { normalizeMcpTelemetryValue, readCallToolNameFromRequestBody, runWithMcpRequestId } from "./requestTelemetry";

/** Local MCP is explicitly enabled and bound to the administrator's existing account. */
function getLocalMcpConfig(): { userId: string; origin: string } | null {
  const enabled = process.env.LOCAL_MCP_ENABLED;
  if (enabled === undefined || enabled === "false") return null;
  if (enabled !== "true" || getAuthConfig().mode !== "local") {
    throw new Error("LOCAL_MCP_ENABLED=true requires AUTH_MODE=local");
  }
  const userId = process.env.LOCAL_MCP_USER_ID ?? "";
  const origin = getConfiguredPublicAppOrigin();
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(userId) || origin === undefined) {
    throw new Error("Local MCP requires LOCAL_MCP_USER_ID and PUBLIC_APP_BASE_URL");
  }
  return { userId, origin };
}

/** Mount inside the normal API prefix. Browser and OAuth authentication remain separate. */
export function createLocalMcpRoutes(): Hono<AppEnv> {
  const app = new Hono<AppEnv>();
  const config = getLocalMcpConfig();
  if (config === null) return app;

  app.all("/mcp", async (c) => {
    c.header("Cache-Control", "no-store");
    const challenge = () => c.json({ error: "A valid local MCP agent key is required" }, 401, {
      "WWW-Authenticate": "Bearer",
    });
    // Native MCP clients omit Origin. Browser callers must use the exact app origin.
    const origin = c.req.header("origin");
    if (origin !== undefined && origin !== config.origin) return c.json({ error: "Invalid origin" }, 403);
    if (c.req.header("host")?.toLowerCase() !== new URL(config.origin).host.toLowerCase()) {
      return c.json({ error: "Invalid host" }, 403);
    }
    const token = /^Bearer\s+(\S+)$/i.exec(c.req.header("authorization")?.trim() ?? "")?.[1];
    if (token === undefined) return challenge();
    let connection;
    try {
      connection = await authenticateAgentApiKey(token);
      if (connection.userId !== config.userId) return challenge();
    } catch (error) {
      if (error instanceof HttpError && error.statusCode === 401) return challenge();
      throw error;
    }
    if (c.req.method !== "POST") return c.json({ error: "Only POST is supported" }, 405, { Allow: "POST" });

    const requestId = c.get("requestId") || randomUUID();
    const startedAtMs = Date.now();
    const caller = normalizeMcpTelemetryValue(c.req.header("user-agent") ?? null);
    const bodyToolName = await readCallToolNameFromRequestBody(c.req.raw);
    let invokedToolName: string | null = null;
    const resourceUrl = `${config.origin}${c.req.path}`;
    const server = createMcpServer(connection, resourceUrl, config.origin, `${config.origin}/icon.svg`, {
      caller, recordInvokedTool: (name) => { invokedToolName = name; },
    });
    const transport = new WebStandardStreamableHTTPServerTransport({
      sessionIdGenerator: undefined,
      enableJsonResponse: true,
      enableDnsRebindingProtection: true,
      allowedHosts: [new URL(config.origin).host],
      allowedOrigins: [config.origin],
    });
    let statusCode = 500;
    let responseChars: number | null = null;
    try {
      await server.connect(transport);
      const response = await runWithMcpRequestId(requestId, () => transport.handleRequest(c.req.raw));
      statusCode = response.status;
      response.headers.set("X-Request-Id", requestId);
      response.headers.set("Cache-Control", "no-store");
      try { responseChars = (await response.clone().text()).length; } catch { /* Telemetry cannot fail the request. */ }
      return response;
    } finally {
      const toolName = invokedToolName ?? bodyToolName;
      try {
        logMcpRequestEvent({
          requestId, httpMethod: c.req.method, userId: connection.userId,
          workspaceId: connection.selectedWorkspaceId, connectionId: connection.connectionId,
          caller, protocolVersion: normalizeMcpTelemetryValue(c.req.header("mcp-protocol-version") ?? null),
          jsonRpcMethod: normalizeMcpTelemetryValue(c.req.header("mcp-method") ?? null),
          toolName, toolExecuted: toolName === null ? null : invokedToolName !== null,
          statusCode, durationMs: Date.now() - startedAtMs, responseChars,
        });
      } catch { /* Telemetry cannot fail the request. */ }
      try { await transport.close(); } finally { await server.close(); }
    }
  });
  return app;
}
