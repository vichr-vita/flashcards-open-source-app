import { query } from "../src/db.js";
/** Real backend/device-auth/Responses HTTP boundary check. The provider is a loopback fixture. */
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { randomUUID } from "node:crypto";

function jwt(exp: number, account = "fixture-account"): string {
  const payload = { exp, email: "fixture@example.test", "https://api.openai.com/auth": { chatgpt_account_id: account, chatgpt_plan_type: "pro" } };
  return `fixture.${Buffer.from(JSON.stringify(payload)).toString("base64url")}.fixture`;
}

export async function startChatGPTFixture() {
  let authorized = false;
  let refreshes = 0;
  let calls = 0;
  let rejectNextCall = false;
  let quotaExceeded = false;
  let rejectNextCatalog = false;
  let toolOutput = "";
  let browserCookies: string | null = null;
  const errors: unknown[] = [];
  const requests: string[] = [];
  const inferenceSelections: Array<{ modelId: string; effort: string | undefined }> = [];
  const models = [
    { slug: "fixture-codex", display_name: "Fixture Codex", visibility: "list", default_reasoning_level: "medium", supported_reasoning_levels: ["low", "medium", "high", "xhigh"].map(effort => ({ effort, description: effort })) },
    { slug: "fixture-second", display_name: "Fixture Second", visibility: "list", default_reasoning_level: "high", supported_reasoning_levels: ["low", "high", "xhigh"].map(effort => ({ effort, description: effort })) },
    { slug: "fixture-plain", display_name: "Fixture Plain", visibility: "list", default_reasoning_level: null, supported_reasoning_levels: [] },
  ];
  const server = createServer(async (request, response) => {
    try {
      let text = "";
      for await (const chunk of request) text += String(chunk);
      const path = request.url ?? "";
      requests.push(path);
      const body = request.headers["content-type"]?.startsWith("application/x-www-form-urlencoded") ? Object.fromEntries(new URLSearchParams(text)) : text === "" ? {} : JSON.parse(text);
      response.setHeader("Content-Type", "application/json");
      if (path === "/fixture-login" && browserCookies !== null) {
        response.setHeader("Set-Cookie", browserCookies.split("; ").map(cookie => `${cookie}; Path=/; SameSite=Lax; ${cookie.startsWith("logged_in=") ? "" : "HttpOnly"}`));
        response.statusCode = 302;
        response.setHeader("Location", "http://localhost:19411");
        response.end();
      } else if (path === "/api/accounts/deviceauth/usercode") {
        assert.equal(body.client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
        response.end(JSON.stringify({ device_auth_id: "fixture-device", user_code: "TEST-1234", interval: "1" }));
      } else if (path === "/api/accounts/deviceauth/token") {
        assert.equal(body.device_auth_id, "fixture-device");
        assert.equal(body.user_code, "TEST-1234");
        if (!authorized) { response.statusCode = 403; response.end("{}"); }
        else response.end(JSON.stringify({ authorization_code: "fixture-code", code_challenge: "fixture-challenge", code_verifier: "fixture-verifier" }));
      } else if (path === "/oauth/token") {
        const expiry = Math.floor(Date.now() / 1000) + 3600;
        if (body.grant_type === "authorization_code") {
          assert.equal(request.headers["content-type"], "application/x-www-form-urlencoded");
          assert.equal(body.code_verifier, "fixture-verifier");
          assert.equal(body.redirect_uri, "https://auth.openai.com/deviceauth/callback");
          response.end(JSON.stringify({ id_token: jwt(expiry), access_token: jwt(expiry), refresh_token: "fixture-refresh" }));
        } else {
          assert.equal(body.grant_type, "refresh_token");
          assert.equal(request.headers["content-type"], "application/json");
          assert.equal(body.refresh_token, "fixture-refresh");
          refreshes += 1;
          // Omitted refresh/id tokens must preserve previous values, as Codex does.
          response.end(JSON.stringify({ access_token: jwt(expiry) }));
        }
      } else if (path.startsWith("/backend-api/codex/models?")) {
        assert.equal(request.headers["chatgpt-account-id"], "fixture-account");
        if (rejectNextCatalog) { rejectNextCatalog = false; response.statusCode = 503; response.end("{}"); return; }
        response.end(JSON.stringify({ models }));
      } else if (path === "/backend-api/codex/responses") {
        assert.equal(request.headers["chatgpt-account-id"], "fixture-account");
        assert.ok(String(request.headers.authorization).startsWith("Bearer fixture."));
        const model = models.find(model => model.slug === body.model);
        assert.ok(model);
        assert.ok(model.supported_reasoning_levels.length === 0 ? body.reasoning.effort === undefined : model.supported_reasoning_levels.some(level => level.effort === body.reasoning.effort));
        inferenceSelections.push({ modelId: body.model, effort: body.reasoning.effort });
        assert.equal(body.stream, true); assert.equal(body.store, false);
        assert.equal(body.max_output_tokens, undefined); assert.equal(body.safety_identifier, undefined);
        assert.ok(body.instructions.length > 0);
        assert.ok(!body.tools.some((tool: { name?: string }) => tool.name === "add_generated_image_to_card"));
        await new Promise(resolve => setTimeout(resolve, 500));
        if (rejectNextCall) { rejectNextCall = false; response.statusCode = 401; response.end('{"error":"fixture-sensitive-error"}'); return; }
        if (quotaExceeded) { response.statusCode = 429; response.end('{"error":"fixture-sensitive-error"}'); return; }
        calls += 1;
        const outputs = body.input.filter((item: { type?: string }) => item.type === "function_call_output");
        const output = outputs.at(-1);
        if (output !== undefined) toolOutput = output.output;
        const item = output === undefined
          ? { type: "function_call", id: "fc_fixture", call_id: "call_fixture", name: "list_workspaces", arguments: "{}", status: "completed" }
          : { type: "message", id: "msg_fixture", role: "assistant", status: "completed", content: [{ type: "output_text", text: "Connected through your ChatGPT subscription.", annotations: [] }] };
        response.setHeader("Content-Type", "text/event-stream");
        const events = [
          { type: "response.output_item.added", output_index: 0, sequence_number: 0, item },
          ...(output === undefined ? [] : [{ type: "response.output_text.delta", item_id: "msg_fixture", output_index: 0, content_index: 0, sequence_number: 1, delta: "Connected through your ChatGPT subscription." }]),
          { type: "response.completed", sequence_number: 2, response: { id: "resp_fixture", object: "response", model: body.model, status: "completed", output: [item], usage: { input_tokens: 100, output_tokens: 25, total_tokens: 125, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } } } },
        ];
        response.end(events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(""));
      } else { response.statusCode = 404; response.end("{}"); }
    } catch (error) { errors.push(error); response.statusCode = 500; response.end("{}"); }
  });
  server.listen(19402, "127.0.0.1");
  await once(server, "listening");
  return {
    authorize: () => { authorized = true; },
    rejectNext: () => { rejectNextCall = true; },
    rejectCatalogNext: () => { rejectNextCatalog = true; },
    exceedQuota: () => { quotaExceeded = true; },
    restoreQuota: () => { quotaExceeded = false; },
    assertHealthy: () => { if (errors.length > 0) throw errors[0]; },
    prepareBrowserReview: (cookies: string) => { browserCookies = cookies; quotaExceeded = false; },
    counts: () => ({ refreshes, calls, toolOutput, requests, inferenceSelections }),
    close: async () => { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); },
  };
}

type Browser = {
  request: (url: string, init?: RequestInit) => Promise<Response>;
  api: (path: string, body: unknown, csrf: string | null, origin?: string) => Promise<Response>;
};
export async function checkChatGPTConnection(browser: Browser, csrf: string, workspaceId: string, directory: string, fixture: Awaited<ReturnType<typeof startChatGPTFixture>>): Promise<void> {
  const apiOrigin = "http://localhost:19400";
  const settingsUrl = `${apiOrigin}/v1/ai/settings`;
  const status = async () => {
    const response = await browser.request(settingsUrl);
    assert.equal(response.status, 200, await response.clone().text());
    assert.equal(response.headers.get("cache-control"), "no-store");
    const text = await response.text();
    assert.ok(!text.includes("accessToken") && !text.includes("refreshToken") && !text.includes("idToken") && !text.includes("fixture-refresh"));
    return JSON.parse(text);
  };
  assert.equal((await fetch(settingsUrl)).status, 401);
  assert.equal((await browser.api("/ai/settings/chatgpt/start", {}, null)).status, 403);
  assert.equal((await browser.api("/ai/settings/chatgpt/start", {}, csrf, "https://untrusted.example")).status, 403);
  const start = await browser.api("/ai/settings/chatgpt/start", {}, csrf);
  assert.equal(start.status, 200, await start.clone().text());
  assert.equal((await start.json()).login.userCode, "TEST-1234");
  assert.equal((await browser.api("/ai/settings", { action: "cancel" }, csrf)).status, 200);
  assert.equal((await status()).login, null);
  await browser.api("/ai/settings/chatgpt/start", {}, csrf);
  fixture.authorize();
  let state;
  for (let attempt = 0; attempt < 15; attempt += 1) {
    state = await status(); if (state.connection !== null) break;
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  assert.equal(state.provider, "chatgpt"); assert.equal(state.connection?.email, "fixture@example.test");
  assert.equal(state.connection.reasoningEffort, "medium");
  assert.deepEqual(state.connection.models[0].supportedReasoningEfforts, ["low", "medium", "high", "xhigh"]);
  assert.equal((await stat(join(directory, "connection.json"))).mode & 0o777, 0o600);
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", modelId: "unavailable" }, csrf)).status, 400);
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", modelId: "fixture-second" }, csrf)).status, 200);
  assert.equal((await status()).connection.reasoningEffort, "high", "Model changes replace unsupported efforts with the model default");
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "medium" }, csrf)).status, 400);
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "invented" }, csrf)).status, 400);
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "" }, csrf)).status, 400);
  assert.equal((await status()).connection.reasoningEffort, "high");
  // Upgrade an existing connection without losing its account, selected model or refresh tokens.
  const connectionPath = join(directory, "connection.json");
  const legacy = JSON.parse(await readFile(connectionPath, "utf8"));
  delete legacy.connection.reasoningEffort;
  legacy.connection.models = legacy.connection.models.map(({ id, name }: { id: string; name: string }) => ({ id, name }));
  legacy.connection.expiresAt = 0;
  await writeFile(connectionPath, JSON.stringify(legacy), { mode: 0o600 });
  fixture.rejectCatalogNext();
  assert.equal((await browser.request(settingsUrl)).status, 502);
  assert.ok(JSON.parse(await readFile(connectionPath, "utf8")).connection.expiresAt > Date.now(), "Refreshed tokens survive a failed catalog upgrade");
  assert.equal((await browser.api("/ai/settings", { action: "api" }, csrf)).status, 200, "A failed catalog upgrade must not block switching to API");
  assert.equal((await status()).provider, "api");
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt" }, csrf)).status, 200);
  const upgraded = (await status()).connection;
  assert.equal(upgraded.modelId, "fixture-second"); assert.equal(upgraded.reasoningEffort, "high");
  assert.equal(fixture.counts().refreshes, 1, "Legacy catalogs also upgrade with expired tokens");
  const migrated = JSON.parse(await readFile(connectionPath, "utf8"));
  assert.equal(migrated.connection.id, legacy.connection.id);
  assert.deepEqual(migrated.connection.models[1].supportedReasoningEfforts, ["low", "high", "xhigh"]);
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "xhigh" }, csrf)).status, 200);
  assert.equal((await status()).connection.reasoningEffort, "xhigh");
  assert.equal(JSON.parse(await readFile(connectionPath, "utf8")).connection.reasoningEffort, "xhigh");
  fixture.rejectNext();
  const turn = { clientRequestId: randomUUID(), workspaceId, content: [{ type: "text", text: "List my workspaces." }], timezone: "Europe/Prague", uiLocale: "en" };
  const started = await browser.api("/chat", turn, csrf);
  assert.equal(started.status, 200, await started.clone().text());
  const run = await started.json();
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "low" }, csrf)).status, 200);
  assert.ok(run.activeRun?.live.stream?.authorization?.startsWith("Live "));
  const url = new URL(run.activeRun?.live.stream.url); url.searchParams.set("sessionId", run.sessionId); url.searchParams.set("runId", run.activeRun.runId);
  const stream = await browser.request(url.toString(), { headers: { Authorization: run.activeRun?.live.stream.authorization }, signal: AbortSignal.timeout(20_000) });
  assert.equal(stream.status, 200, await stream.clone().text());
  assert.equal(stream.headers.get("content-type"), "text/event-stream");
  const events = await stream.text();
  fixture.assertHealthy();
  assert.ok(events.includes("Connected through your ChatGPT subscription."), events);
  assert.ok(events.includes('"outcome":"completed"'), events);
  assert.ok(fixture.counts().toolOutput.includes(workspaceId), fixture.counts().toolOutput);
  assert.equal(fixture.counts().refreshes, 2);
  assert.equal(fixture.counts().calls, 2);
  assert.ok(fixture.counts().inferenceSelections.every(selection => selection.modelId === "fixture-second" && selection.effort === "xhigh"), "A running turn keeps its effort through token refresh and tool follow-ups");
  assert.equal((await status()).connection.reasoningEffort, "low", "Token refresh preserves the selection for the next turn");
  const usage = await query<{ model_id: string; user_supplied_key: boolean }>("SELECT model_id, user_supplied_key FROM ai.usage_events WHERE workspace_id = $1 AND surface = 'chat'", [workspaceId]);
  assert.equal(usage.rows.length, 2);
  assert.ok(usage.rows.every(event => event.model_id === "fixture-second" && event.user_supplied_key), "Subscription usage must retain its actual model and bypass platform-funded limits");
  // The API-key setting remains irrelevant while the subscription is selected.
  const saved = JSON.parse(await readFile(join(directory, "connection.json"), "utf8"));
  saved.connection.expiresAt = 0;
  await writeFile(join(directory, "connection.json"), JSON.stringify(saved), { mode: 0o600 });
  fixture.exceedQuota();
  const capped = await browser.api("/chat", { ...turn, sessionId: run.sessionId, clientRequestId: randomUUID() }, csrf);
  assert.equal(capped.status, 200, await capped.clone().text());
  const cappedRun = await capped.json();
  const cappedUrl = new URL(cappedRun.activeRun.live.stream.url); cappedUrl.searchParams.set("sessionId", cappedRun.sessionId); cappedUrl.searchParams.set("runId", cappedRun.activeRun.runId);
  const cappedStream = await browser.request(cappedUrl.toString(), { headers: { Authorization: cappedRun.activeRun.live.stream.authorization }, signal: AbortSignal.timeout(20_000) });
  const cappedEvents = await cappedStream.text();
  assert.ok(cappedEvents.includes('"outcome":"error"') || cappedEvents.includes('"outcome":"reset_required"'), cappedEvents);
  const failedRun = await query<{ status: string; last_error_message: string }>("SELECT status, last_error_message FROM ai.chat_runs WHERE run_id = $1", [cappedRun.activeRun.runId]);
  assert.equal(failedRun.rows[0]?.status, "failed");
  assert.ok(failedRun.rows[0]?.last_error_message.includes("AI settings"));
  assert.equal(fixture.counts().refreshes, 3, "Expired tokens are refreshed before inference");
  assert.equal(fixture.counts().inferenceSelections.at(-1)?.effort, "low", "New turns use the updated effort");
  assert.ok(!cappedEvents.includes("fixture-sensitive-error"));
  fixture.restoreQuota();
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", modelId: "fixture-plain" }, csrf)).status, 200);
  assert.equal((await status()).connection.reasoningEffort, null);
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "low" }, csrf)).status, 400);
  const plain = await browser.api("/chat", { ...turn, clientRequestId: randomUUID() }, csrf);
  assert.equal(plain.status, 200, await plain.clone().text());
  const plainRun = await plain.json();
  const plainUrl = new URL(plainRun.activeRun.live.stream.url); plainUrl.searchParams.set("sessionId", plainRun.sessionId); plainUrl.searchParams.set("runId", plainRun.activeRun.runId);
  const plainStream = await browser.request(plainUrl.toString(), { headers: { Authorization: plainRun.activeRun.live.stream.authorization }, signal: AbortSignal.timeout(20_000) });
  const plainEvents = await plainStream.text();
  fixture.assertHealthy();
  assert.ok(plainEvents.includes('"outcome":"completed"') || plainEvents.includes('"outcome":"reset_required"'), plainEvents);
  // A fast reply can finish before live attachment; its persisted result remains authoritative.
  let plainStatus;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const result = await query<{ status: string; last_error_message: string | null }>("SELECT status, last_error_message FROM ai.chat_runs WHERE run_id = $1", [plainRun.activeRun.runId]);
    plainStatus = result.rows[0];
    if (plainStatus?.status === "completed" || plainStatus?.status === "failed") break;
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  assert.equal(plainStatus?.status, "completed", plainStatus?.last_error_message ?? plainEvents);
  assert.deepEqual(fixture.counts().inferenceSelections.at(-1), { modelId: "fixture-plain", effort: undefined }, "Models without effort support omit the API-funded effort");
  assert.equal((await browser.api("/ai/settings", { action: "disconnect" }, csrf)).status, 200);
  assert.equal((await status()).provider, "chatgpt", "Disconnect must not silently switch to a billed provider");
  assert.equal((await browser.api("/chat", { ...turn, clientRequestId: randomUUID() }, csrf)).status, 409);
  assert.equal((await browser.api("/ai/settings", { action: "api" }, csrf)).status, 200);
  assert.ok(!(await readFile(join(directory, "connection.json"), "utf8")).includes("fixture-refresh"));
  fixture.assertHealthy();
  console.log("Passed device-code cancellation/login, CSRF, private token storage, model/effort selection, legacy connection upgrade, refresh, real chat tool execution with captured effort, local SSE, usage failure, and disconnect without API fallback.");
}
