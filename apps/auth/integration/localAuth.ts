import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { startChatGPTFixture, checkChatGPTConnection } from "./chatgpt.js";
/** Real HTTP/PostgreSQL integration. Run only against the disposable container in the self-hosting guide. */
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import pg from "pg";
import type { AuthenticationResponseJSON, PublicKeyCredentialCreationOptionsJSON, PublicKeyCredentialRequestOptionsJSON } from "@simplewebauthn/server";
import { TestPasskey } from "./passkey.js";
import { closeDatabase, query } from "../src/db.js";
import { bootstrapAccount, issueEnrollment, revokePasskey, revokeAllSessions } from "../src/local/admin.js";
import { hashToken, newToken } from "../src/local/credentials.js";

const connectionDirectory = await mkdtemp(join(tmpdir(), "nibomo-chatgpt-integration-"));
const fixture = await startChatGPTFixture();
const ownerUrl = "postgresql://flashcards_owner@127.0.0.1:19432/flashcards";
const authOrigin = "http://localhost:19401";
const webOrigin = "http://localhost:19411";
const apiOrigin = "http://localhost:19400";
const root = fileURLToPath(new URL("../../../", import.meta.url));
Object.assign(process.env, {
  CHATGPT_CONNECTION_DIR: connectionDirectory, LOCAL_CHATGPT_FIXTURE: "true", CHAT_LIVE_URL: "http://localhost:19400/v1/chat/live",
  COOKIE_DOMAIN: "localhost", AUTH_MODE: "local", NODE_ENV: "development", LOCAL_AUTH_ALLOW_HTTP: "true",
  DATABASE_URL: ownerUrl, WEBAUTHN_RP_ID: "localhost",
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
  async openLogin(enrollment = false): Promise<void> {
    const response = await this.request(enrollment ? `${authOrigin}/enroll` : `${authOrigin}/login?redirect_uri=${encodeURIComponent(webOrigin)}`);
    assert.equal(response.status, 200);
    const html = await response.text();
    assert.ok(html.includes(enrollment ? "Create passkey" : "Sign in with passkey"));
    assert.ok(!html.includes("password") && !html.includes("Authenticator code"));
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(response.headers.get("referrer-policy"), "no-referrer");
    const config = html.match(/id="local-auth-config" type="application\/json">([^<]+)</u)?.[1];
    assert.ok(config); this.csrf = (JSON.parse(config) as { csrfToken: string }).csrfToken;
  }
  async ceremony(path: string, body: unknown): Promise<Response> {
    return this.request(`${authOrigin}/api/webauthn/${path}`, { method: "POST", headers: { Origin: authOrigin, "Content-Type": "application/json", "X-CSRF-Token": this.csrf }, body: JSON.stringify(body) });
  }
  async options(): Promise<PublicKeyCredentialRequestOptionsJSON> {
    const response = await this.ceremony("authentication/options", {});
    assert.equal(response.status, 200, await response.clone().text());
    const options = await response.json() as PublicKeyCredentialRequestOptionsJSON;
    assert.equal(options.userVerification, "required"); assert.equal(options.rpId, "localhost");
    return options;
  }
  async login(key: TestPasskey): Promise<Response> { return this.ceremony("authentication/verify", key.assertion(await this.options())); }
  async enroll(url: string, key: TestPasskey): Promise<void> {
    await this.openLogin(true);
    const grant = new URLSearchParams(new URL(url).hash.slice(1)).get("enroll"); assert.ok(grant);
    const response = await this.ceremony("registration/options", { grant });
    assert.equal(response.status, 200, await response.clone().text());
    const options = await response.json() as PublicKeyCredentialCreationOptionsJSON;
    assert.equal(options.authenticatorSelection?.userVerification, "required");
    assert.equal(options.authenticatorSelection?.residentKey, "required");
    assert.equal(options.authenticatorSelection?.authenticatorAttachment, undefined);
    const verified = await this.ceremony("registration/verify", { grant, credential: key.registration(options) });
    assert.equal(verified.status, 200, await verified.clone().text());
    assert.equal(verified.headers.getSetCookie().length, 0, "Enrollment cannot issue a session");
    assert.equal((await this.ceremony("registration/options", { grant })).status, 401, "Grant is single-use");
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

const key = new TestPasskey();
const syncedKey = new TestPasskey(true);
const first = new BrowserSession();
const second = new BrowserSession();
let userId: string | null = null;
try {
  const existing = await query("SELECT 1 FROM auth.local_account", []);
  assert.equal(existing.rows.length, 0, "Disposable test database must have no local account");
  const createdAccount = await bootstrapAccount(); userId = createdAccount.userId;
  await assert.rejects(bootstrapAccount(), /already exists/);
  let auth = start("apps/auth/dist/index.js", "auth_app", 19401);
  let backend = start("apps/backend/dist/entrypoints/index.js", "backend_app", 19400);
  await Promise.all([ready(`${authOrigin}/health`), ready(`${apiOrigin}/v1/health`)]);
  await first.openLogin();
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  assert.equal((await first.ceremony("registration/options", { grant: newToken() })).status, 401);
  assert.equal((await first.ceremony("authentication/options", {})).status, 401);
  await first.openLogin(true);
  const bootstrapGrant = new URLSearchParams(new URL(createdAccount.enrollmentUrl).hash.slice(1)).get("enroll");
  for (const override of [{ flags: 0x41 }, { origin: webOrigin }, { rpId: "untrusted.example" }, { crossOrigin: true }]) {
    const optionsResponse = await first.ceremony("registration/options", { grant: bootstrapGrant });
    assert.equal(optionsResponse.status, 200);
    const options = await optionsResponse.json() as PublicKeyCredentialCreationOptionsJSON;
    assert.equal((await first.ceremony("registration/verify", { grant: bootstrapGrant, credential: key.registration(options, override) })).status, 401);
    assert.equal((await query("SELECT 1 FROM auth.local_webauthn_challenges WHERE challenge_hash = $1", [hashToken(options.challenge)])).rows.length, 0);
    assert.equal((await query("SELECT 1 FROM auth.local_passkeys", [])).rows.length, 0);
    await query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL", []);
  }
  await first.enroll(createdAccount.enrollmentUrl, key);
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  assert.equal((await query("SELECT 1 FROM auth.local_sessions", [])).rows.length, 0);
  await second.enroll(await issueEnrollment(false), syncedKey);
  assert.equal((await query("SELECT 1 FROM auth.local_passkeys", [])).rows.length, 2);
  await second.openLogin();
  const loginEndpoint = `${authOrigin}/api/webauthn/authentication/options`;
  for (const headers of ([{ Origin: "https://untrusted.example", "X-CSRF-Token": first.csrf }, { Origin: authOrigin }] as Array<Record<string, string>>)) {
    assert.equal((await first.request(loginEndpoint, { method: "POST", headers: { ...headers, "Content-Type": "application/json" }, body: "{}" })).status, 403);
  }
  for (const override of [{ wrongSignature: true }, { origin: webOrigin }, { rpId: "untrusted.example" }, { flags: 0x01 }, { flags: 0x04 }, { crossOrigin: true }, { challenge: newToken() }, { userHandle: newToken() }]) {
    const options = await first.options();
    assert.equal((await first.ceremony("authentication/verify", key.assertion(options, override))).status, 401);
    await query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL", []);
  }
  const bound = key.assertion(await first.options());
  assert.equal((await second.ceremony("authentication/verify", bound)).status, 401, "Challenge is bound to its requesting browser");
  const expired = await first.options();
  await query("UPDATE auth.local_webauthn_challenges SET expires_at = now() - interval '1 second' WHERE challenge_hash = $1", [hashToken(expired.challenge)]);
  assert.equal((await first.ceremony("authentication/verify", key.assertion(expired))).status, 401);
  await query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL", []);
  const replay = key.assertion(await first.options());
  const sameBrowserCookies = first.cookieHeader();
  const concurrent = await Promise.all([first.ceremony("authentication/verify", replay), fetch(`${authOrigin}/api/webauthn/authentication/verify`, { method: "POST", headers: { Origin: authOrigin, "Content-Type": "application/json", "X-CSRF-Token": first.csrf, Cookie: sameBrowserCookies }, body: JSON.stringify(replay) })]);
  assert.deepEqual(concurrent.map(response => response.status).sort(), [200, 401]);
  // Capture the winning cookie pair even when the raw fetch won the lock.
  for (const cookie of concurrent.find(response => response.ok)!.headers.getSetCookie()) { const pair = cookie.split(";")[0]; const index = pair.indexOf("="); first.cookies.set(pair.slice(0, index), pair.slice(index + 1)); }
  const loginResponse = concurrent.find(response => response.ok)!;
  assert.ok(loginResponse.headers.getSetCookie().filter(cookie => /^(session|refresh)=/.test(cookie)).every(cookie => cookie.includes("HttpOnly") && cookie.includes("SameSite=Lax")));
  const me = await first.me(); assert.equal(me.userId, userId);
  await query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL", []);
  const failedOptions = await second.options();
  const validAfterFailure = key.assertion(failedOptions);
  const failed = structuredClone(validAfterFailure);
  const badSignature = Buffer.from(failed.response.signature, "base64url"); badSignature[badSignature.length - 1] ^= 1;
  failed.response.signature = badSignature.toString("base64url");
  assert.equal((await second.ceremony("authentication/verify", failed)).status, 401);
  assert.equal((await second.ceremony("authentication/verify", validAfterFailure)).status, 401, "Rejected completed assertion consumes the challenge");
  for (let attempt = 0; attempt < 3; attempt++) await second.ceremony("authentication/verify", key.assertion(await second.options(), { wrongSignature: true }));
  const throttled = await second.ceremony("authentication/options", {});
  assert.equal(throttled.status, 429); assert.ok(Number(throttled.headers.get("retry-after")) > 0);
  await stop(auth); auth = start("apps/auth/dist/index.js", "auth_app", 19401); await ready(`${authOrigin}/health`);
  assert.equal((await second.ceremony("authentication/options", {})).status, 429, "Throttle survives restart");
  await query("UPDATE auth.local_account SET locked_until = now() - interval '1 second'", []);
  assert.equal((await second.login(syncedKey)).status, 200);
  const secondMe = await second.me();
  assert.equal(secondMe.userId, me.userId); assert.equal(secondMe.selectedWorkspaceId, me.selectedWorkspaceId);
  console.log("Passed owner-only enrollment, device verification, real signature/origin/RP checks, single-use browser-bound challenges, replay rejection, persistent throttling, and stable identity.");

  for (const scheme of ["Bearer", "Guest", "ApiKey"]) {
    assert.equal((await first.request(`${apiOrigin}/v1/me`, { headers: { Authorization: `${scheme} ${newToken()}` } })).status, 401);
  }
  for (const path of ["/api/login", "/api/send-code", "/api/verify-code", "/api/agent/send-code", "/api/refresh-token", "/token", "/register"]) {
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

  if (process.env.LOCAL_AUTH_BROWSER_SMOKE === "true") {
    // Hand only this disposable account's session to the real browser flow.
    const statePath = join(connectionDirectory, "browser-session.json");
    await writeFile(statePath, JSON.stringify({ cookies: [...first.cookies].map(([name, value]) => ({
      name, value, domain: "localhost", path: "/", expires: -1,
      httpOnly: true, secure: false, sameSite: "Lax",
    })), origins: [] }), { mode: 0o600 });
    const browser = spawn(process.execPath, ["node_modules/@playwright/test/cli.js", "test", "--config=playwright.local-account.config.ts"], {
      cwd: join(root, "apps/web"), env: { ...process.env, LOCAL_AUTH_BROWSER_STATE: statePath }, stdio: "inherit",
    });
    children.add(browser);
    const [code] = await once(browser, "exit");
    children.delete(browser);
    assert.equal(code, 0, "Local-account Playwright smoke failed");
  }

  await checkChatGPTConnection(first, me.csrfToken, me.selectedWorkspaceId, connectionDirectory, fixture);

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
  const enrollmentBrowser = new BrowserSession();
  const expiredGrantUrl = await issueEnrollment(false);
  const expiredGrant = new URLSearchParams(new URL(expiredGrantUrl).hash.slice(1)).get("enroll");
  await query("UPDATE auth.local_enrollment_grants SET expires_at = now() - interval '1 second'", []);
  await enrollmentBrowser.openLogin(true);
  assert.equal((await enrollmentBrowser.ceremony("registration/options", { grant: expiredGrant })).status, 401);
  const additional = new TestPasskey();
  await enrollmentBrowser.enroll(await issueEnrollment(false), additional);
  await enrollmentBrowser.openLogin(); assert.equal((await enrollmentBrowser.login(additional)).status, 200);
  await revokePasskey(additional.id);
  assert.equal((await enrollmentBrowser.request(`${apiOrigin}/v1/me`)).status, 401);
  await enrollmentBrowser.openLogin();
  assert.equal((await enrollmentBrowser.login(additional)).status, 401, "Revoked credential cannot authenticate");
  await first.openLogin(); assert.equal((await first.login(key)).status, 200);
  const resetUrl = await issueEnrollment(true);
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  const replacement = new TestPasskey(true);
  await first.enroll(resetUrl, replacement);
  assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  assert.equal((await first.login(key)).status, 401, "Reset invalidates previous passkeys");
  assert.equal((await first.login(replacement)).status, 200);
  assert.equal((await first.me()).selectedWorkspaceId, me.selectedWorkspaceId);
  await revokeAllSessions(); assert.equal((await first.request(`${apiOrigin}/v1/me`)).status, 401);
  await first.openLogin(); assert.equal((await first.login(replacement)).status, 200, "Zero-counter synced passkey remains usable");
  const backendClient = new pg.Client({ connectionString: ownerUrl.replace("flashcards_owner@", "backend_app@") });
  const authClient = new pg.Client({ connectionString: ownerUrl.replace("flashcards_owner@", "auth_app@") });
  try {
    await Promise.all([backendClient.connect(), authClient.connect()]);
    await assert.rejects(backendClient.query("SELECT public_key FROM auth.local_passkeys"), /permission denied/);
    await assert.rejects(backendClient.query("SELECT refresh_hash FROM auth.local_sessions"), /permission denied/);
    await assert.rejects(authClient.query("UPDATE auth.local_passkeys SET public_key = decode('00', 'hex')"), /permission denied/);
    await assert.rejects(authClient.query("INSERT INTO auth.local_enrollment_grants (grant_hash, user_id, expires_at) VALUES ($1, $2, now())", [hashToken(newToken()), userId]), /permission denied/);
    await assert.rejects(authClient.query("DELETE FROM auth.local_passkeys"), /permission denied/);
  } finally { await Promise.all([backendClient.end(), authClient.end()]); }
  if (process.env.LOCAL_CHATGPT_BROWSER_REVIEW === "true") {
    fixture.prepareBrowserReview(first.cookieHeader());
    console.log("Disposable browser review ready at http://localhost:19402/fixture-login. Press Enter to finish and clean up.");
    await once(process.stdin, "data");
  }
  await query("DELETE FROM org.workspaces WHERE workspace_id IN (SELECT workspace_id FROM org.workspace_memberships WHERE user_id = $1)", [userId]);
  await query("DELETE FROM org.user_settings WHERE user_id = $1", [userId]); userId = null;
  assert.equal((await query("SELECT 1 FROM auth.local_account", [])).rows.length, 0);
  await assert.rejects(issueEnrollment(true), /does not exist/);
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
  await fixture.close();
  await rm(connectionDirectory, { recursive: true, force: true });
}
