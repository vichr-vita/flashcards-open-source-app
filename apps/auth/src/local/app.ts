import { readFile } from "node:fs/promises";
import type { Context } from "hono";
import { timingSafeEqual } from "node:crypto";
import { Hono } from "hono";
import { getCookie, setCookie, deleteCookie } from "hono/cookie";
import { bodyLimit } from "hono/body-limit";
import { query } from "../db.js";
import { clearBrowserSessionCookies, setBrowserSessionCookies } from "../server/browserSession.js";
import { getLocalAuthConfig } from "./config.js";
import { newToken } from "./credentials.js";
import { renderLocalLoginPage } from "./loginPage.js";
import { authenticationOptions, authenticatePasskey, registrationOptions, registerPasskey, isCredentialResponse } from "./webauthn.js";
import { refresh, revoke, verifySession } from "./store.js";

export function createLocalAuthApp(basePath: string): Hono {
  const config = getLocalAuthConfig();
  const app = new Hono().basePath(basePath);
  const csrfCookieOptions = { path: "/", httpOnly: true, secure: !config.allowHttp, sameSite: "Strict" as const, maxAge: 600 };
  app.use("*", async (c, next) => {
    c.header("Cache-Control", "no-store");
    c.header("X-Robots-Tag", "noindex, nofollow, noarchive");
    c.header("X-Content-Type-Options", "nosniff");
    c.header("Referrer-Policy", "no-referrer");
    c.header("Permissions-Policy", "publickey-credentials-get=(self), publickey-credentials-create=(self)");
    await next();
  });
  app.onError(() => appError());
  function appError() { return new Response(JSON.stringify({ error: "Authentication is temporarily unavailable. Try again." }), { status: 503, headers: { "Content-Type": "application/json", "Cache-Control": "no-store" } }); }

  // Require an attributable browser origin even when CORS would hide the response.
  app.use("/api/*", async (c, next) => {
    const origin = c.req.header("origin");
    if (!origin || ![config.authOrigin, ...config.redirectOrigins].includes(origin) || c.req.header("sec-fetch-site") === "cross-site") {
      return c.json({ error: "Origin is not allowed" }, 403);
    }
    c.header("Access-Control-Allow-Origin", origin);
    c.header("Access-Control-Allow-Credentials", "true");
    c.header("Vary", "Origin");
    if (c.req.method === "OPTIONS") {
      c.header("Access-Control-Allow-Methods", "POST, OPTIONS");
      c.header("Access-Control-Allow-Headers", "content-type, x-csrf-token, sentry-trace, baggage");
      return c.body(null, 204);
    }
    await next();
  });
  app.use("/api/webauthn/*", bodyLimit({ maxSize: 32768 }));
  app.use("/api/webauthn/*", async (c, next) => {
    const expected = Buffer.from(getCookie(c, "local_login_csrf") ?? "");
    const supplied = Buffer.from(c.req.header("x-csrf-token") ?? "");
    if (c.req.header("origin") !== config.authOrigin || expected.length !== 43 || expected.length !== supplied.length || !timingSafeEqual(expected, supplied)) {
      return c.json({ error: "Sign-in page expired. Reload to try again.", code: "LOGIN_CSRF_INVALID" }, 403);
    }
    if (c.req.header("content-type")?.split(";")[0] !== "application/json") return c.json({ error: "JSON is required" }, 415);
    await next();
  });

  function allowedRedirect(value: string | undefined): string | null {
    try {
      const url = new URL(value ?? "");
      return !url.username && !url.password && config.redirectOrigins.includes(url.origin) ? url.toString() : null;
    } catch { return null; }
  }

  app.get("/health", async c => { await query("SELECT 1", []); return c.json({ ok: true, authMode: "local" }); });
  app.get("/robots.txt", c => c.text("User-agent: *\nDisallow: /\n"));
  app.get("/assets/local-passkey.js", async c => {
    const file = new URL(import.meta.url.endsWith(".ts") ? "../../dist/local/passkey-browser.js" : "./passkey-browser.js", import.meta.url);
    c.header("Content-Type", "text/javascript; charset=utf-8");
    return c.body(await readFile(file, "utf8"));
  });
  for (const path of ["/login", "/enroll"]) {
    app.get(path, async c => {
      const enrollment = path === "/enroll";
      const redirectUri = allowedRedirect(c.req.query("redirect_uri") ?? (enrollment ? config.redirectOrigins[0] : undefined));
      if (!redirectUri) return c.text("Invalid redirect_uri", 400);
      if (!enrollment && await verifySession(getCookie(c, "session") ?? "")) return c.redirect(redirectUri);
      const csrfToken = newToken();
      const nonce = newToken();
      setCookie(c, "local_login_csrf", csrfToken, csrfCookieOptions);
      c.header("Content-Security-Policy", `default-src 'none'; img-src data:; style-src 'nonce-${nonce}'; script-src 'nonce-${nonce}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'`);
      return c.html(renderLocalLoginPage(csrfToken, redirectUri, nonce, enrollment));
    });
  }
  async function requestBody(c: Context): Promise<unknown> {
    try { return await c.req.json<unknown>(); } catch { return null; }
  }
  function failure(c: Context, result: { status: "invalid" } | { status: "throttled"; retryAfter: number }, enrollment = false) {
    if (result.status === "throttled") {
      c.header("Retry-After", String(result.retryAfter));
      return c.json({ error: `Too many attempts. Try again in ${result.retryAfter} seconds.`, code: "LOGIN_THROTTLED" }, 429);
    }
    return c.json(enrollment ? { error: "This link has expired. Use a new setup link.", code: "ENROLLMENT_INVALID" } : { error: "Passkey sign-in failed. Try again." }, 401);
  }
  app.post("/api/webauthn/authentication/options", async c => {
    const body = await requestBody(c);
    if (!body || typeof body !== "object" || Array.isArray(body)) return c.json({ error: "Invalid request" }, 400);
    const result = await authenticationOptions(getCookie(c, "local_login_csrf")!);
    return result.status === "valid" ? c.json(result.options) : failure(c, result);
  });
  app.post("/api/webauthn/authentication/verify", async c => {
    const body = await requestBody(c);
    if (!isCredentialResponse(body, "authentication")) return c.json({ error: "Invalid passkey response" }, 400);
    const result = await authenticatePasskey(getCookie(c, "local_login_csrf")!, body);
    if (result.status !== "valid") return failure(c, result);
    setBrowserSessionCookies(c, result.sessionToken, result.refreshToken);
    deleteCookie(c, "local_login_csrf", csrfCookieOptions);
    return c.json({ ok: true });
  });
  for (const action of ["options", "verify"] as const) {
    app.post(`/api/webauthn/registration/${action}`, async c => {
      const body = await requestBody(c);
      if (!body || typeof body !== "object" || !("grant" in body) || typeof body.grant !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(body.grant)) return c.json({ error: "Invalid setup request" }, 400);
      const browserToken = getCookie(c, "local_login_csrf")!;
      if (action === "options") {
        const result = await registrationOptions(browserToken, body.grant);
        return result.status === "valid" ? c.json(result.options) : failure(c, result, true);
      }
      if (!("credential" in body) || !isCredentialResponse(body.credential, "registration")) return c.json({ error: "Invalid passkey response" }, 400);
      const result = await registerPasskey(browserToken, body.grant, body.credential);
      return result.status === "valid" ? c.json({ ok: true }) : failure(c, result, true);
    });
  }
  app.post("/api/refresh-session", async c => {
    const refreshToken = getCookie(c, "refresh") ?? "";
    const sessionToken = await refresh(refreshToken, getCookie(c, "session") ?? "");
    if (!sessionToken) { clearBrowserSessionCookies(c); return c.json({ error: "Sign in again." }, 401); }
    setBrowserSessionCookies(c, sessionToken, refreshToken);
    return c.json({ ok: true });
  });
  for (const path of ["/logout", "/logout-local"]) {
    app.get(path, async c => {
      const redirectUri = allowedRedirect(c.req.query("redirect_uri"));
      if (!redirectUri) return c.text("Invalid redirect_uri", 400);
      let origin = c.req.header("origin");
      try { origin ??= new URL(c.req.header("referer") ?? "").origin; } catch { /* Refuse unattributed navigation below. */ }
      if (!origin || ![config.authOrigin, ...config.redirectOrigins].includes(origin) || c.req.header("sec-fetch-site") === "cross-site") return c.text("Origin is not allowed", 403);
      await revoke(getCookie(c, "session") ?? "", getCookie(c, "refresh") ?? "");
      clearBrowserSessionCookies(c);
      deleteCookie(c, "local_login_csrf", csrfCookieOptions);
      const url = new URL(redirectUri);
      url.searchParams.set("logged_out", "1");
      if (path === "/logout-local") url.searchParams.set("account_deleted", "1");
      return c.redirect(url.toString());
    });
  }
  // No email OTP, demo-account bypass, native tokens, OAuth, or agent issuance is mounted here.
  return app;
}
