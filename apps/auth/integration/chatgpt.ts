import { query } from "../src/db.js";
/** Real backend/device-auth/Responses HTTP boundary check. The provider is a loopback fixture. */
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createHmac, randomUUID } from "node:crypto";

type ToolReply = { tool: string; data: { card?: { cardId: string; backText?: string }; backText?: string } };
type FixtureOutput = { type: "function_call"; id: string; call_id: string; name: string; arguments: string; status: string } | { type: "message"; id: string; role: string; status: string; content: Array<{ type: string; text: string; annotations: never[] }> };

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
  const parityReviewId = randomUUID();
  const parityOutputs: ToolReply[] = [];
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
      const body = request.headers["content-type"]?.startsWith("application/x-www-form-urlencoded") ? Object.fromEntries(new URLSearchParams(text)) : request.headers["content-type"]?.startsWith("multipart/form-data") ? { multipart: text } : text === "" ? {} : JSON.parse(text);
      response.setHeader("Content-Type", "application/json");
      if (path === "/fixture-login" && browserCookies !== null) {
        response.setHeader("Set-Cookie", browserCookies.split("; ").map(cookie => `${cookie}; Path=/; SameSite=Lax; ${cookie.startsWith("logged_in=") ? "" : "HttpOnly"}`));
        response.statusCode = 302;
        response.setHeader("Location", "http://localhost:19411");
        response.end();
      } else if (path === "/v1/audio/transcriptions") {
        assert.equal(request.headers.authorization, "Bearer fixture-own-key");
        assert.ok(body.multipart.includes('name="file"') && body.multipart.includes("gpt-4o-transcribe"));
        response.end(JSON.stringify({ text: "A transcript to review before sending.", usage: { type: "tokens", input_tokens: 9, output_tokens: 7 } }));
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
        let item: FixtureOutput = output === undefined
          ? { type: "function_call", id: "fc_fixture", call_id: "call_fixture", name: "list_workspaces", arguments: "{}", status: "completed" }
          : { type: "message", id: "msg_fixture", role: "assistant", status: "completed", content: [{ type: "output_text", text: "Connected through your ChatGPT subscription.", annotations: [] }] };
        const latestUser = body.input.filter((item: { role?: string }) => item.role === "user").at(-1);
        const parity = latestUser?.content.some((part: { text?: string }) => part.text === "Create and review the parity card.");
        if (parity) {
          const completed = outputs.filter((item: { call_id?: string }) => item.call_id?.startsWith("parity-"));
          const result = completed.at(-1) === undefined ? null : JSON.parse(completed.at(-1).output);
          if (result !== null) { assert.equal(result.ok, true, completed.at(-1).output); parityOutputs.push(result); }
          const cardId = parityOutputs.find(output => output.tool === "next_review_card")?.data.card?.cardId;
          const steps = [
            ["sql_execute", { sql: "INSERT INTO cards (front_text, back_text, tags) VALUES ('What does ownership manage?', 'Memory lifetime.', ('rust-chat-parity')) RETURNING card_id, front_text, back_text" }],
            ["next_review_card", { tags: ["rust-chat-parity"] }],
            ["reveal_answer", { cardId }],
            ["submit_review", { cardId, reviewId: parityReviewId, rating: "Good", reviewedTimeZone: "Europe/Prague" }],
          ] as const;
          const step = steps[completed.length];
          item = step === undefined
            ? { type: "message", id: "msg_parity", role: "assistant", status: "completed", content: [{ type: "output_text", text: "Created and reviewed the parity card.", annotations: [] }] }
            : { type: "function_call", id: `fc_parity-${completed.length}`, call_id: `parity-${completed.length}`, name: step[0], arguments: JSON.stringify(step[1]), status: "completed" };
        }
        response.setHeader("Content-Type", "text/event-stream");
        const events = [
          { type: "response.output_item.added", output_index: 0, sequence_number: 0, item },
          ...(item.type !== "message" ? [] : [{ type: "response.output_text.delta", item_id: item.id, output_index: 0, content_index: 0, sequence_number: 1, delta: item.content?.[0]?.text }]),
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
    counts: () => ({ refreshes, calls, toolOutput, requests, inferenceSelections, parityOutputs, parityReviewId }),
    close: async () => { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); },
  };
}

type Browser = {
  request: (url: string, init?: RequestInit) => Promise<Response>;
  api: (path: string, body: unknown, csrf: string | null, origin?: string) => Promise<Response>;
  restartBackend?: () => Promise<void>;
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
  const malformedCancel = await browser.api("/ai/settings", { action: "cancel", unexpected: true }, csrf);
  assert.equal(malformedCancel.status, 400);
  assert.equal((await malformedCancel.json()).code, "AI_SETTINGS_INVALID");
  assert.equal((await status()).login.userCode, "TEST-1234", "A malformed cancellation cannot clear pending sign-in");
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
  const originalConnectionFile = await readFile(join(directory, "connection.json"), "utf8");
  for (const malformed of [
    { action: "disconnect", unexpected: true },
    { action: "api", modelId: "" },
    { action: "cancel", modelId: "x".repeat(201) },
    { action: "disconnect", reasoningEffort: "x".repeat(101) },
    { action: "chatgpt", modelId: null },
    { action: "api", reasoningEffort: 7 },
    { action: "unknown" },
  ]) {
    const response = await browser.api("/ai/settings", malformed, csrf);
    assert.equal(response.status, 400);
    assert.equal((await response.json()).code, "AI_SETTINGS_INVALID");
    assert.equal(await readFile(join(directory, "connection.json"), "utf8"), originalConnectionFile, "Malformed settings cannot save, switch, or disconnect the connection");
  }
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
  if (process.env.RUST_AI_SETTINGS_ONLY === "true") {
    assert.equal((await browser.api("/ai/settings", { action: "disconnect" }, csrf)).status, 200);
    assert.equal((await status()).provider, "chatgpt");
    assert.equal((await browser.api("/ai/settings", { action: "api" }, csrf)).status, 200);
    assert.ok(!(await readFile(connectionPath, "utf8")).includes("fixture-refresh"));
    fixture.assertHealthy();
    console.log("Passed Rust device-code cancellation/login, CSRF, private token storage, model/effort selection, legacy connection upgrade, refresh, and disconnect.");
    return;
  }
  fixture.rejectNext();
  const turn = { clientRequestId: randomUUID(), workspaceId, content: [{ type: "text", text: "List my workspaces." }], timezone: "Europe/Prague", uiLocale: "en" };
  const started = await browser.api("/chat", turn, csrf);
  assert.equal(started.status, 200, await started.clone().text());
  const run = await started.json();
  assert.equal((await browser.api("/ai/settings", { action: "chatgpt", reasoningEffort: "low" }, csrf)).status, 200);
  assert.ok(run.activeRun?.live.stream?.authorization?.startsWith("Live "));
  const url = new URL(run.activeRun?.live.stream.url); url.searchParams.set("sessionId", run.sessionId); url.searchParams.set("runId", run.activeRun.runId);
  if (process.env.RUST_STACK_BINARY !== undefined) {
    const forged = `${run.activeRun.live.stream.authorization.slice(0, -1)}!`;
    assert.equal((await browser.request(url.toString(), { headers: { Authorization: forged } })).status, 401);
    assert.equal((await fetch(url, { headers: { Authorization: run.activeRun.live.stream.authorization } })).status, 401);
    const payload = Buffer.from(JSON.stringify({ version: 1, userId: legacy.userId, workspaceId, sessionId: run.sessionId, runId: run.activeRun.runId, expiresAt: Date.now() + 600_000, traceContext: null })).toString("base64url");
    const csrfSecret = process.env.BACKEND_CSRF_SECRET;
    assert.ok(csrfSecret);
    const secret = createHmac("sha256", csrfSecret).update("local-chat-live-v1").digest("hex");
    const signature = createHmac("sha256", secret).update(payload).digest("base64url");
    run.activeRun.live.stream.authorization = `Live ${payload}.${signature}`;
  }
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
  if (process.env.RUST_STACK_BINARY !== undefined) {
    const recording = new FormData();
    recording.append("file", new Blob(["fixture WAV recording"], { type: "audio/wav" }), "recording.wav");
    recording.append("source", "web"); recording.append("workspaceId", workspaceId); recording.append("sessionId", plainRun.sessionId);
    const transcript = await browser.request(`${apiOrigin}/v1/chat/transcriptions`, { method: "POST", headers: { Origin: "http://localhost:19411", "X-CSRF-Token": csrf, "X-OpenAI-Api-Key": "fixture-own-key" }, body: recording });
    assert.equal(transcript.status, 200, await transcript.clone().text());
    assert.deepEqual(await transcript.json(), { text: "A transcript to review before sending.", sessionId: plainRun.sessionId });
    const dictationUsage = await query<{ input_tokens: string; output_tokens: string; user_supplied_key: boolean }>("SELECT input_tokens,output_tokens,user_supplied_key FROM ai.usage_events WHERE workspace_id=$1 AND surface='dictation'", [workspaceId]);
    assert.deepEqual(dictationUsage.rows, [{ input_tokens: "9", output_tokens: "7", user_supplied_key: true }]);
    const response = await browser.api("/chat", { ...turn, sessionId: randomUUID(), clientRequestId: randomUUID(), content: [{ type: "text", text: "Create and review the parity card." }] }, csrf);
    assert.equal(response.status, 200, await response.clone().text());
    const parityRun = await response.json();
    const liveUrl = new URL(parityRun.activeRun.live.stream.url);
    liveUrl.searchParams.set("sessionId", parityRun.sessionId); liveUrl.searchParams.set("runId", parityRun.activeRun.runId);
    const live = await browser.request(liveUrl.toString(), { headers: { Authorization: parityRun.activeRun.live.stream.authorization }, signal: AbortSignal.timeout(20_000) });
    const events = await live.text();
    fixture.assertHealthy();
    assert.ok(events.includes('"outcome":"completed"'), events);
    const outputs = fixture.counts().parityOutputs;
    assert.deepEqual(outputs.map(output => output.tool), ["sql_execute", "next_review_card", "reveal_answer", "submit_review"]);
    assert.equal(outputs[1]?.data.card?.backText, undefined, "Review selection must not reveal the answer");
    assert.equal(outputs[2]?.data.backText, "Memory lifetime.");
    const persisted = await query<{ front_text: string; back_text: string; reps: number; client_event_id: string }>("SELECT c.front_text,c.back_text,c.reps,e.client_event_id FROM content.cards c JOIN content.review_events e USING(workspace_id,card_id) WHERE c.workspace_id=$1 AND c.card_id=$2", [workspaceId, outputs[1]?.data.card?.cardId]);
    assert.equal(persisted.rows.length, 1);
    assert.equal(persisted.rows[0]?.front_text, "What does ownership manage?");
    assert.equal(persisted.rows[0]?.back_text, "Memory lifetime.");
    assert.equal(persisted.rows[0]?.reps, 1);
    assert.equal(persisted.rows[0]?.client_event_id, `agent-review:${fixture.counts().parityReviewId}`);
    const history = await browser.request(`${apiOrigin}/v1/chat?sessionId=${parityRun.sessionId}&workspaceId=${workspaceId}`);
    assert.equal(history.status, 200);
    const snapshot = await history.json();
    assert.equal(snapshot.activeRun, null);
    assert.equal(snapshot.conversation.mainContentInvalidationVersion, 2);
    for (const part of snapshot.conversation.messages.at(-1).content) {
      if (part.streamPosition === undefined) continue;
      assert.ok("contentIndex" in part.streamPosition && "sequenceNumber" in part.streamPosition);
    }
    const stopped = await browser.api("/chat", { ...turn, sessionId: randomUUID(), clientRequestId: randomUUID() }, csrf);
    assert.equal(stopped.status, 200, await stopped.clone().text());
    const stoppingRun = await stopped.json();
    const stop = await browser.api("/chat/stop", { workspaceId, sessionId: stoppingRun.sessionId, runId: stoppingRun.activeRun.runId }, csrf);
    assert.equal(stop.status, 200); assert.equal((await stop.json()).stopped, true);
    await new Promise(resolve => setTimeout(resolve, 1500));
    const stoppedHistory = await browser.request(`${apiOrigin}/v1/chat?sessionId=${stoppingRun.sessionId}&workspaceId=${workspaceId}`);
    const stoppedSnapshot = await stoppedHistory.json();
    assert.equal(stoppedSnapshot.activeRun, null);
    assert.equal(stoppedSnapshot.conversation.messages.at(-1).isStopped, true);
    const restart = await browser.api("/chat", { ...turn, sessionId: randomUUID(), clientRequestId: randomUUID() }, csrf);
    assert.equal(restart.status, 200, await restart.clone().text());
    const interruptedRun = await restart.json();
    assert.ok(browser.restartBackend);
    await browser.restartBackend();
    await query("UPDATE ai.chat_runs SET worker_heartbeat_at=now()-interval '1 minute' WHERE run_id=$1", [interruptedRun.activeRun.runId]);
    const recovered = await browser.request(`${apiOrigin}/v1/chat?sessionId=${interruptedRun.sessionId}&workspaceId=${workspaceId}`);
    assert.equal(recovered.status, 200);
    const recoveredSnapshot = await recovered.json();
    assert.equal(recoveredSnapshot.activeRun, null);
    assert.equal(recoveredSnapshot.conversation.messages.at(-1).isError, true);
    assert.ok(recoveredSnapshot.conversation.messages.at(-1).content.some((part: { text?: string }) => part.text?.includes("interrupted")));
    assert.equal((await status()).connection.modelId, "fixture-plain", "Connection and model selection survive process restart");
    console.log("Passed Rust legacy Live signatures, SQL card creation and FSRS review tools, dictation usage, cancellation, and actual backend restart recovery.");
  }
  assert.equal((await browser.api("/ai/settings", { action: "disconnect" }, csrf)).status, 200);
  assert.equal((await status()).provider, "chatgpt", "Disconnect must not silently switch to a billed provider");
  assert.equal((await browser.api("/chat", { ...turn, clientRequestId: randomUUID() }, csrf)).status, 409);
  assert.equal((await browser.api("/ai/settings", { action: "api" }, csrf)).status, 200);
  assert.ok(!(await readFile(join(directory, "connection.json"), "utf8")).includes("fixture-refresh"));
  fixture.assertHealthy();
  console.log("Passed device-code cancellation/login, CSRF, private token storage, model/effort selection, legacy connection upgrade, refresh, real chat tool execution with captured effort, local SSE, usage failure, and disconnect without API fallback.");
}
