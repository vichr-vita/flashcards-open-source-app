import { timingSafeEqual } from "node:crypto";
import { Hono } from "hono";
import { getCookie, setCookie, deleteCookie } from "hono/cookie";
import { bodyLimit } from "hono/body-limit";
import { query } from "../db.js";
import { clearBrowserSessionCookies, setBrowserSessionCookies } from "../server/browserSession.js";
import { getLocalAuthConfig } from "./config.js";
import { newToken } from "./credentials.js";
import { renderLocalLoginPage } from "./loginPage.js";
import { login, refresh, revoke, verifySession } from "./store.js";

export function createLocalAuthApp(basePath: string): Hono {
  const config = getLocalAuthConfig();
  const app = new Hono().basePath(basePath);
  const csrfCookieOptions = { path: "/", httpOnly: true, secure: !config.allowHttp, sameSite: "Strict" as const, maxAge: 600 };
  app.use("*", async (c, next) => {
    c.header("Cache-Control", "no-store");
    c.header("X-Robots-Tag", "noindex, nofollow, noarchive");
    c.header("X-Content-Type-Options", "nosniff");
    c.header("Referrer-Policy", "same-origin");
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
  app.use("/api/login", bodyLimit({ maxSize: 4096 }));

  function allowedRedirect(value: string | undefined): string | null {
    try {
      const url = new URL(value ?? "");
      return !url.username && !url.password && config.redirectOrigins.includes(url.origin) ? url.toString() : null;
    } catch { return null; }
  }

  app.get("/health", async c => { await query("SELECT 1", []); return c.json({ ok: true, authMode: "local" }); });
  app.get("/robots.txt", c => c.text("User-agent: *\nDisallow: /\n"));
  app.get("/login", async c => {
    const redirectUri = allowedRedirect(c.req.query("redirect_uri"));
    if (!redirectUri) return c.text("Invalid redirect_uri", 400);
    if (await verifySession(getCookie(c, "session") ?? "")) return c.redirect(redirectUri);
    const csrfToken = newToken();
    const nonce = newToken();
    setCookie(c, "local_login_csrf", csrfToken, csrfCookieOptions);
    c.header("Content-Security-Policy", `default-src 'none'; style-src 'nonce-${nonce}'; script-src 'nonce-${nonce}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'`);
    return c.html(renderLocalLoginPage(csrfToken, redirectUri, nonce));
  });
  app.post("/api/login", async c => {
    const cookieToken = getCookie(c, "local_login_csrf") ?? "";
    const headerToken = c.req.header("x-csrf-token") ?? "";
    const expected = Buffer.from(cookieToken);
    const supplied = Buffer.from(headerToken);
    if (c.req.header("origin") !== config.authOrigin || expected.length !== 43 || expected.length !== supplied.length || !timingSafeEqual(expected, supplied)) {
      return c.json({ error: "Sign-in page expired.", code: "LOGIN_CSRF_INVALID" }, 403);
    }
    if (c.req.header("content-type")?.split(";")[0] !== "application/json") return c.json({ error: "JSON is required" }, 415);
    let body: unknown;
    try { body = await c.req.json<unknown>(); } catch { return c.json({ error: "Invalid login request" }, 400); }
    if (!body || typeof body !== "object" || !("password" in body) || !("code" in body) || typeof body.password !== "string" || typeof body.code !== "string" || body.password.length === 0 || Buffer.byteLength(body.password) > 1024 || !/^\d{6}$/.test(body.code)) {
      return c.json({ error: "Enter your password and a 6-digit code." }, 400);
    }
    const result = await login(body.password, body.code);
    if (result.status === "throttled") {
      c.header("Retry-After", String(result.retryAfter));
      return c.json({ error: `Too many attempts. Try again in ${result.retryAfter} seconds.`, code: "LOGIN_THROTTLED" }, 429);
    }
    if (result.status === "replayed") return c.json({ error: "Code already used. Wait for the next code." }, 401);
    if (result.status !== "valid") return c.json({ error: "Password or code is incorrect. Try again." }, 401);
    setBrowserSessionCookies(c, result.sessionToken, result.refreshToken);
    deleteCookie(c, "local_login_csrf", csrfCookieOptions);
    return c.json({ ok: true });
  });
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
