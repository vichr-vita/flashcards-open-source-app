/** Real HTTP/PostgreSQL integration. Run only against the disposable container in the self-hosting guide. */
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import pg from "pg";
import { closeDatabase, query } from "../src/db.js";
import { bootstrapAccount, resetCredentials, revokeAllSessions } from "../src/local/admin.js";
import { createAuthenticator, hashToken, newToken } from "../src/local/credentials.js";

const ownerUrl = "postgresql://flashcards_owner@127.0.0.1:19432/flashcards";
const authOrigin = "http://localhost:19401";
const webOrigin = "http://localhost:19410";
const apiOrigin = "http://localhost:19400";
const root = fileURLToPath(new URL("../../../", import.meta.url));
Object.assign(process.env, {
  COOKIE_DOMAIN: "localhost", AUTH_MODE: "local", NODE_ENV: "development", LOCAL_AUTH_ALLOW_HTTP: "true",
  DATABASE_URL: ownerUrl, LOCAL_AUTH_ENCRYPTION_KEY: Buffer.from(newToken()).subarray(0, 32).toString("base64"),
  PUBLIC_AUTH_BASE_URL: authOrigin, ALLOWED_REDIRECT_URIS: webOrigin,
  PUBLIC_APP_BASE_URL: webOrigin, BACKEND_ALLOWED_ORIGINS: webOrigin,
  BACKEND_CSRF_SECRET: newToken(), AWS_EC2_METADATA_DISABLED: "true",
});
for (const name of ["DB_SECRET_ARN", "COGNITO_USER_POOL_ID", "COGNITO_CLIENT_ID", "COGNITO_REGION", "DEMO_EMAIL_DOSTIP", "DEMO_PASSWORD_DOSTIP", "SENTRY_DSN", "LANGFUSE_SECRET_KEY"]) delete process.env[name];

const children = new Set<ChildProcess>();
let logs = "";
let unexpectedOutboundDetected = false;
function start(entrypoint: string, username: string, port: number): ChildProcess {
  const url = new URL(ownerUrl); url.username = username;
  const child = spawn(process.execPath, ["--import", "./apps/auth/integration/noEgress.ts", entrypoint], { cwd: root, env: { ...process.env, DATABASE_URL: url.toString(), PORT: String(port) }, stdio: ["ignore", "pipe", "pipe"] });
  children.add(child);
  for (const stream of [child.stdout, child.stderr]) stream?.on("data", data => {
    const chunk = String(data);
    unexpectedOutboundDetected ||= chunk.includes("Unexpected outbound network request");
    logs = (logs + chunk).slice(-8000);
  });
  return child;
}
async function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode === null && child.signalCode === null) { child.kill(); await once(child, "exit"); }
  children.delete(child);
}
async function ready(url: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt++) {
    try { if ((await fetch(url)).ok) return; } catch { /* Wait for the child server. */ }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error(`Server did not start: ${url}\n${logs}`);
}

class BrowserSession {
  readonly cookies = new Map<string, string>();
  csrf = "";
  cookieHeader(): string { return [...this.cookies].map(([key, value]) => `${key}=${value}`).join("; "); }
  async request(url: string, init: RequestInit = {}): Promise<Response> {
    const headers = new Headers(init.headers);
    headers.set("Cookie", this.cookieHeader());
    const response = await fetch(url, { ...init, headers, redirect: "manual" });
    for (const cookie of response.headers.getSetCookie()) {
      const pair = cookie.split(";")[0] ?? "";
      const separator = pair.indexOf("=");
      const name = pair.slice(0, separator); const value = pair.slice(separator + 1);
      if (value === "" || /Max-Age=0/i.test(cookie)) this.cookies.delete(name); else this.cookies.set(name, value);
    }
    return response;
  }
  async openLogin(): Promise<void> {
    const response = await this.request(`${authOrigin}/login?redirect_uri=${encodeURIComponent(webOrigin)}`);
    assert.equal(response.status, 200);
    const html = await response.text();
    assert.ok(html.includes("Authenticator code"));
    assert.equal(response.headers.get("cache-control"), "no-store");
    const token = html.match(/const csrfToken = "([A-Za-z0-9_-]+)"/u)?.[1];
    assert.ok(token); this.csrf = token;
  }
  async login(password: string, code: string): Promise<Response> {
    return this.request(`${authOrigin}/api/login`, { method: "POST", headers: { Origin: authOrigin, "Content-Type": "application/json", "X-CSRF-Token": this.csrf }, body: JSON.stringify({ password, code }) });
  }
  async me() {
    const response = await this.request(`${apiOrigin}/v1/me`);
    assert.equal(response.status, 200, await response.clone().text());
    const body = await response.json() as { userId: string; selectedWorkspaceId: string; csrfToken: string; authTransport: string; profile: { email: string | null } };
    assert.equal(body.authTransport, "session"); assert.equal(body.profile.email, null);
    assert.ok(body.csrfToken); assert.ok(body.selectedWorkspaceId);
    return body;
  }
  async api(path: string, body: unknown, csrf: string | null, origin = webOrigin): Promise<Response> {
    return this.request(`${apiOrigin}/v1${path}`, { method: "POST", headers: { Origin: origin, "Content-Type": "application/json", ...(csrf === null ? {} : { "X-CSRF-Token": csrf }) }, body: JSON.stringify(body) });
  }
}

const authenticator = createAuthenticator();
const password = newToken();
const first = new BrowserSession();
const second = new BrowserSession();
let userId: string | null = null;
try {
  const existing = await query("SELECT 1 FROM auth.local_account", []);
  assert.equal(existing.rows.length, 0, "Disposable test database must have no local account");
  await assert.rejects(bootstrapAccount(password, authenticator.secret, "invalid"));
  userId = await bootstrapAccount(password, authenticator.secret, authenticator.generate());
  await assert.rejects(bootstrapAccount(password, authenticator.secret, authenticator.generate()), /already exists/);
  // Enrollment consumes its confirmation code. Advance the fixture's enrollment marker, not server time.
  await query("UPDATE auth.local_account SET last_totp_counter = -1", []);
  let auth = start("apps/auth/dist/index.js", "auth_app", 19401);
  let backend = start("apps/backend/dist/entrypoints/index.js", "backend_app", 19400);
  await Promise.all([ready(`${authOrigin}/health`), ready(`${apiOrigin}/v1/health`)]);
  await first.openLogin();
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  assert.equal((await first.login("incorrect-password", authenticator.generate())).status, 401);
  assert.equal((await first.login(password, "not-a-code")).status, 400);
  const timestampForWrongCode = Date.now();
  const acceptedCodes = new Set([-1, 0, 1].map(delta => authenticator.generate({ timestamp: timestampForWrongCode + delta * 30000 })));
  let wrongCode = "000000";
  while (acceptedCodes.has(wrongCode)) wrongCode = String(Number(wrongCode) + 1).padStart(6, "0");
  assert.equal((await first.login(password, wrongCode)).status, 401);
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  assert.equal((await query("SELECT 1 FROM auth.local_sessions", [])).rows.length, 0);
  assert.equal((await first.request(`${authOrigin}/api/login`, { method: "POST", headers: { Origin: "https://untrusted.example", "Content-Type": "application/json" }, body: JSON.stringify({ password, code: authenticator.generate() }) })).status, 403);
  assert.equal((await first.request(`${authOrigin}/api/login`, { method: "POST", headers: { Origin: authOrigin, "Content-Type": "application/json" }, body: JSON.stringify({ password, code: authenticator.generate() }) })).status, 403);
  const validCode = authenticator.generate();
  const loginResponse = await first.login(password, validCode);
  assert.equal(loginResponse.status, 200);
  assert.ok(loginResponse.headers.getSetCookie().filter(cookie => /^(session|refresh)=/.test(cookie)).every(cookie => cookie.includes("HttpOnly") && cookie.includes("SameSite=Lax")));
  const me = await first.me();
  assert.equal(me.userId, userId);
  await second.openLogin();
  assert.equal((await second.login(password, validCode)).status, 401);
  for (let attempt = 0; attempt < 3; attempt++) await second.login("incorrect-password", validCode);
  const throttled = await second.login("incorrect-password", validCode);
  assert.equal(throttled.status, 429); assert.ok(Number(throttled.headers.get("retry-after")) > 0);
  assert.equal((await second.login(password, authenticator.generate({ timestamp: Date.now() + 30000 }))).status, 429);
  await stop(auth); auth = start("apps/auth/dist/index.js", "auth_app", 19401); await ready(`${authOrigin}/health`);
  assert.equal((await second.login(password, validCode)).status, 429, "Throttle survives a service restart");
  await query("UPDATE auth.local_account SET locked_until = now() - interval '1 second'", []);
  assert.equal((await second.login(password, authenticator.generate({ timestamp: Date.now() + 30000 }))).status, 200);
  const secondMe = await second.me();
  assert.equal(secondMe.userId, me.userId); assert.equal(secondMe.selectedWorkspaceId, me.selectedWorkspaceId);
  console.log("Passed password/TOTP, replay rejection, persistent throttling, login CSRF, and stable identity.");

  for (const scheme of ["Bearer", "Guest", "ApiKey"]) {
    assert.equal((await first.request(`${apiOrigin}/v1/me`, { headers: { Authorization: `${scheme} ${newToken()}` } })).status, 401);
  }
  for (const path of ["/api/send-code", "/api/verify-code", "/api/agent/send-code", "/api/refresh-token", "/token", "/register"]) {
    assert.equal((await first.request(`${authOrigin}${path}`, { method: "POST", headers: { Origin: authOrigin } })).status, 404);
  }
  for (const path of ["/guest-sessions", "/agent-api-keys", "/agent/sql/query", "/admin/users"]) {
    assert.equal((await first.api(path, {}, me.csrfToken)).status, 404);
  }
  assert.equal((await first.api("/me/delete", { confirmationText: "delete my account" }, me.csrfToken)).status, 409);

  const installationId = randomUUID(); const cardId = randomUUID(); const timestamp = new Date().toISOString();
  const pushPath = `/workspaces/${me.selectedWorkspaceId}/sync/push`;
  const card = { cardId, frontText: "What is the capital of Czechia?", backText: "Prague", cardType: "basic", tags: [], effortLevel: "fast", dueAt: null, createdAt: timestamp, reps: 0, lapses: 0, fsrsCardState: "new", fsrsStepIndex: null, fsrsStability: null, fsrsDifficulty: null, fsrsLastReviewedAt: null, fsrsScheduledDays: null, deletedAt: null };
  const push = { installationId, platform: "web", appVersion: "local-auth-integration", operations: [{ operationId: randomUUID(), entityType: "card", entityId: cardId, action: "upsert", clientUpdatedAt: timestamp, payload: card }] };
  assert.equal((await first.api(pushPath, push, null)).status, 403);
  assert.equal((await first.api(pushPath, push, "invalid")).status, 403);
  assert.equal((await first.api(pushPath, push, me.csrfToken, "https://untrusted.example")).status, 403);
  const created = await first.api(pushPath, push, me.csrfToken);
  assert.equal(created.status, 200, await created.clone().text());
  const reviewId = randomUUID(); const reviewedAt = new Date().toISOString();
  // The web client syncs the review event together with a snapshot computed by the shared scheduler.
  const scheduler = await import("../../backend/dist/scheduling/index.js");
  const { defaultWorkspaceSchedulerConfig } = await import("../../backend/dist/scheduling/workspaceConfig.js");
  const nextSchedule = scheduler.computeReviewSchedule(scheduler.createEmptyReviewableCardScheduleState(cardId), defaultWorkspaceSchedulerConfig, 3, new Date(reviewedAt));
  const reviewedCard = { ...card, ...nextSchedule, dueAt: nextSchedule.dueAt.toISOString(), fsrsLastReviewedAt: nextSchedule.fsrsLastReviewedAt.toISOString() };
  const reviewed = await first.api(pushPath, { ...push, operations: [
    { operationId: randomUUID(), entityType: "review_event", entityId: reviewId, action: "append", clientUpdatedAt: reviewedAt, payload: { reviewEventId: reviewId, cardId, clientEventId: randomUUID(), rating: 3, reviewedAtClient: reviewedAt, reviewedTimeZone: "Europe/Prague" } },
    { operationId: randomUUID(), entityType: "card", entityId: cardId, action: "upsert", clientUpdatedAt: reviewedAt, payload: reviewedCard },
  ] }, me.csrfToken);
  assert.equal(reviewed.status, 200, await reviewed.clone().text());
  const pullPath = `/workspaces/${me.selectedWorkspaceId}/sync/pull`;
  const pull = { installationId: randomUUID(), platform: "web", appVersion: "local-auth-integration", afterHotChangeId: 0, limit: 100, includeMediaAssets: false };
  const pulled = await second.api(pullPath, pull, secondMe.csrfToken);
  assert.equal(pulled.status, 200, await pulled.clone().text());
  assert.ok((await pulled.text()).includes(cardId));
  const schedule = await query<{ reps: number; due_at: Date }>("SELECT reps, due_at FROM content.cards WHERE card_id = $1", [cardId]);
  assert.equal(schedule.rows[0]?.reps, 1); assert.ok(schedule.rows[0]?.due_at);
  await stop(backend); backend = start("apps/backend/dist/entrypoints/index.js", "backend_app", 19400); await ready(`${apiOrigin}/v1/health`);
  assert.equal((await first.me()).selectedWorkspaceId, me.selectedWorkspaceId);
  const afterRestart = await second.api(pullPath, { ...pull, installationId: randomUUID() }, secondMe.csrfToken);
  assert.equal(afterRestart.status, 200); assert.ok((await afterRestart.text()).includes(cardId));
  console.log("Passed backend CSRF, disabled alternate auth, card creation, review scheduling, cross-session sync, and restart persistence.");

  const savedSession = first.cookies.get("session"); assert.ok(savedSession);
  await query("UPDATE auth.local_sessions SET expires_at = now() - interval '1 second' WHERE session_hash = $1", [hashToken(savedSession)]);
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  assert.equal((await first.request(`${authOrigin}/api/refresh-session`, { method: "POST", headers: { Origin: webOrigin } })).status, 200);
  assert.equal((await first.me()).userId, userId);
  const concurrentRefreshes = await Promise.all(Array.from({ length: 3 }, () => first.request(`${authOrigin}/api/refresh-session`, { method: "POST", headers: { Origin: webOrigin } })));
  assert.ok(concurrentRefreshes.every(response => response.ok)); assert.equal((await first.me()).csrfToken, me.csrfToken);
  const refreshOnly = new BrowserSession(); refreshOnly.cookies.set("refresh", first.cookies.get("refresh") ?? "");
  assert.equal((await refreshOnly.request(`${authOrigin}/api/refresh-session`, { method: "POST", headers: { Origin: webOrigin } })).status, 401);
  assert.equal((await first.request(`${authOrigin}/api/refresh-session`, { method: "POST", headers: { Origin: "https://untrusted.example" } })).status, 403);
  const logoutUrl = `${authOrigin}/logout?redirect_uri=${encodeURIComponent(webOrigin)}`;
  assert.equal((await first.request(logoutUrl)).status, 403);
  assert.equal((await first.request(logoutUrl, { headers: { Referer: webOrigin } })).status, 302);
  assert.equal((await fetch(`${apiOrigin}/v1/me`, { headers: { Cookie: `session=${savedSession}` } })).status, 401);
  await query("UPDATE auth.local_sessions SET expires_at = now() - interval '2 seconds', refresh_expires_at = now() - interval '1 second'", []);
  assert.equal((await second.request(`${authOrigin}/api/refresh-session`, { method: "POST", headers: { Origin: webOrigin } })).status, 401);
  assert.equal((await second.request(`${apiOrigin}/v1/me`)).status, 401);
  const nextPassword = newToken(); await resetCredentials({ password: nextPassword });
  await first.openLogin();
  assert.equal((await first.login(password, validCode)).status, 401);
  const nextAuthenticator = createAuthenticator();
  await resetCredentials({ secret: nextAuthenticator.secret, code: nextAuthenticator.generate() });
  assert.equal((await first.login(nextPassword, authenticator.generate())).status, 401);
  await second.openLogin();
  const concurrentCode = nextAuthenticator.generate({ timestamp: Date.now() + 30000 });
  const concurrentLogins = await Promise.all([first.login(nextPassword, concurrentCode), second.login(nextPassword, concurrentCode)]);
  assert.deepEqual(concurrentLogins.map(response => response.status).sort(), [200, 401], "A code can establish exactly one session under concurrent login");
  const winner = concurrentLogins[0].ok ? first : second;
  assert.equal((await winner.me()).userId, userId);
  await resetCredentials({ password: newToken() });
  assert.equal((await winner.request(`${apiOrigin}/v1/me`)).status, 401);
  await revokeAllSessions();
  const backendClient = new pg.Client({ connectionString: ownerUrl.replace("flashcards_owner@", "backend_app@") });
  const authClient = new pg.Client({ connectionString: ownerUrl.replace("flashcards_owner@", "auth_app@") });
  try {
    await Promise.all([backendClient.connect(), authClient.connect()]);
    await assert.rejects(backendClient.query("SELECT password_hash FROM auth.local_account"), /permission denied/);
    await assert.rejects(backendClient.query("SELECT refresh_hash FROM auth.local_sessions"), /permission denied/);
    await assert.rejects(authClient.query("UPDATE auth.local_account SET password_hash = 'forbidden'"), /permission denied/);
  } finally { await Promise.all([backendClient.end(), authClient.end()]); }
  await query("DELETE FROM org.workspaces WHERE workspace_id IN (SELECT workspace_id FROM org.workspace_memberships WHERE user_id = $1)", [userId]);
  await query("DELETE FROM org.user_settings WHERE user_id = $1", [userId]); userId = null;
  assert.equal((await query("SELECT 1 FROM auth.local_account", [])).rows.length, 0);
  await assert.rejects(resetCredentials({ password: newToken() }), /does not exist/);
  assert.ok(!unexpectedOutboundDetected, "Browser auth must not attempt Cognito/email-service calls");
  console.log("Passed expiry, concurrent refresh, logout, absolute expiry, recovery/reset revocation, deletion cascade, and runtime-role isolation.");
} finally {
  await Promise.all([...children].map(stop));
  // Only this fixture's account is cleaned up; this script refuses a pre-existing account.
  if (userId) {
    await query("DELETE FROM org.workspaces WHERE workspace_id IN (SELECT workspace_id FROM org.workspace_memberships WHERE user_id = $1)", [userId]);
    await query("DELETE FROM org.user_settings WHERE user_id = $1", [userId]);
  }
  await closeDatabase();
}
