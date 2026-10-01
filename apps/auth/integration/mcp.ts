import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { randomUUID } from "node:crypto";
import { request as httpRequest } from "node:http";

const run = promisify(execFile);

/** Exercises the real local MCP transport, runtime DB role, tools, and owner key lifecycle. */
export async function checkLocalMcp(root: string, userId: string, workspaceId: string): Promise<void> {
  const keyCommand = async (...args: string[]) => {
    const result = await run(process.execPath, ["apps/backend/scripts/local-mcp-key.cjs", ...args], { cwd: root, env: process.env });
    return JSON.parse(result.stdout) as { userId: string; apiKey: string; connection: { connectionId: string } };
  };
  const created = await keyCommand("create", "MCP integration");
  assert.equal(created.userId, userId);
  // Node's fetch rewrites Host. Use HTTP directly to test the reverse proxy's public Host.
  const request = (payload: unknown, token: string | null = created.apiKey, extra: Record<string, string> = {}) => new Promise<Response>((resolve, reject) => {
    const req = httpRequest("http://localhost:19400/v1/mcp", { method: "POST", headers: {
      Host: "localhost:19411", "Content-Type": "application/json", Accept: "application/json, text/event-stream",
      ...(token === null ? {} : { Authorization: `Bearer ${token}` }), ...extra,
    } }, response => {
      let body = "";
      response.setEncoding("utf8");
      response.on("data", chunk => { body += chunk; });
      response.on("error", reject);
      response.on("end", () => {
        const headers = new Headers();
        for (const [name, value] of Object.entries(response.headers)) {
          if (value !== undefined) headers.set(name, Array.isArray(value) ? value.join(", ") : value);
        }
        resolve(new Response(body, { status: response.statusCode, headers }));
      });
    });
    req.on("error", reject);
    req.end(JSON.stringify(payload));
  });
  const call = async (name: string, args: Record<string, unknown>) => {
    const response = await request({ jsonrpc: "2.0", id: randomUUID(), method: "tools/call", params: { name, arguments: args } });
    assert.equal(response.status, 200, await response.clone().text());
    const rpc = await response.json() as { result: { isError?: boolean; content: Array<{ text: string }> } };
    assert.equal(rpc.result.isError, undefined, JSON.stringify(rpc));
    return rpc.result.content[0].text;
  };
  try {
    const unauthenticated = await request({}, null);
    assert.equal(unauthenticated.status, 401, await unauthenticated.text());
    assert.equal((await request({}, "invalid")).status, 401);
    assert.equal((await request({}, created.apiKey, { Origin: "https://untrusted.example" })).status, 403);
    assert.equal((await request({}, created.apiKey, { Host: "untrusted.example" })).status, 403);
    const initialized = await request({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "local-mcp-integration", version: "1.0.0" } } });
    assert.equal(initialized.status, 200, await initialized.clone().text());
    assert.equal(initialized.headers.get("cache-control"), "no-store");
    const listed = await request({ jsonrpc: "2.0", id: 2, method: "tools/list" });
    assert.equal(listed.status, 200);
    const inventory = await listed.json() as { result: { tools: Array<{ name: string }> } };
    assert.equal(inventory.result.tools.length, 8);
    assert.ok((await call("list_workspaces", {})).includes(workspaceId));
    assert.ok((await call("get_guide", { topic: "sql_dialect" })).includes("cards"));
    const inserted = JSON.parse(await call("sql_execute", { workspaceId, sql: "INSERT INTO cards (front_text, back_text, tags) VALUES ('Which city is the Czech capital?', 'Prague', ('mcp-integration'))" })) as { data: { rows: Array<{ card_id: string }> } };
    const cardId = inserted.data.rows[0].card_id;
    assert.ok((await call("sql_query", { workspaceId, sql: `SELECT card_id, front_text FROM cards WHERE card_id = '${cardId}'` })).includes(cardId));
    await call("submit_review", { workspaceId, cardId, reviewId: randomUUID(), rating: "Good", reviewedTimeZone: "Europe/Prague" });
    for (const path of ["me", "agent/sql/query", "agent-api-keys"]) {
      const response = await fetch(`http://localhost:19400/v1/${path}`, { headers: { Authorization: `Bearer ${created.apiKey}` } });
      assert.equal(response.status, path === "me" ? 401 : 404);
    }
    await keyCommand("revoke", created.connection.connectionId);
    assert.equal((await request({ jsonrpc: "2.0", id: 3, method: "tools/list" })).status, 401);
    console.log("Passed local MCP authentication, host/origin protection, tool discovery, scoped reads/writes, reviews, browser-route isolation, and immediate revocation.");
  } finally {
    await keyCommand("revoke", created.connection.connectionId);
  }
}
