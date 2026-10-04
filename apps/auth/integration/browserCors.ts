import assert from "node:assert/strict";
import { request } from "node:http";

// Fetch derives Host from its URL; the HTTP client preserves the wire header needed here.
function postWithHost(url: string, webOrigin: string, host: string, body?: string): Promise<{ status: number; cookies: readonly string[] }> {
  return new Promise((resolve, reject) => {
    const outgoing = request(url, {
      method: "POST", headers: { Origin: webOrigin, Host: host, "Content-Type": "application/json" },
    }, incoming => {
      incoming.on("error", reject);
      incoming.resume();
      incoming.on("end", () => resolve({ status: incoming.statusCode ?? 0, cookies: incoming.headers["set-cookie"] ?? [] }));
    });
    outgoing.on("error", reject);
    outgoing.end(body);
  });
}

/** Verify the deployed browser contract against the running HTTP server. */
export async function verifyBrowserCors(apiOrigin: string, webOrigin: string): Promise<void> {
  const requestHeaders = [
    "content-type", "authorization", "x-csrf-token", "sentry-trace", "baggage",
    "x-chat-request-id", "x-chat-resume-attempt-id", "x-chat-live-client-id",
    "x-client-platform", "x-client-version", "x-media-asset-id", "x-media-source-url",
    "x-media-created-at", "x-media-client-updated-at", "x-media-last-modified-by-replica-id",
    "x-media-last-operation-id", "x-package-media-key", "x-openai-api-key",
  ];
  const responseHeaders = [
    "cache-control", "content-disposition", "content-encoding", "content-length",
    "content-type", "x-request-id", "x-amz-apigw-id", "x-amzn-requestid",
    "x-chat-request-id", "retry-after",
  ];
  const preflight = await fetch(`${apiOrigin}/v1/chat/live`, {
    method: "OPTIONS",
    headers: {
      Origin: webOrigin,
      "Access-Control-Request-Method": "GET",
      "Access-Control-Request-Headers": requestHeaders.join(","),
    },
  });
  assert.ok(preflight.ok, "A trusted browser can preflight the live endpoint");
  assert.equal(preflight.headers.get("access-control-allow-origin"), webOrigin);
  assert.equal(preflight.headers.get("access-control-allow-credentials"), "true");
  const allowed = new Set(preflight.headers.get("access-control-allow-headers")?.toLowerCase().split(",").map(value => value.trim()));
  for (const header of requestHeaders) assert.ok(allowed.has(header), `Browser request header ${header} is allowed`);

  const health = await fetch(`${apiOrigin}/v1/health`, { headers: { Origin: webOrigin } });
  assert.ok(health.ok);
  assert.equal(health.headers.get("access-control-allow-origin"), webOrigin);
  assert.equal(health.headers.get("access-control-allow-credentials"), "true");
  const exposed = new Set(health.headers.get("access-control-expose-headers")?.toLowerCase().split(",").map(value => value.trim()));
  for (const header of responseHeaders) assert.ok(exposed.has(header), `Browser response header ${header} is exposed`);

  const untrusted = await fetch(`${apiOrigin}/v1/chat/live`, {
    method: "OPTIONS",
    headers: {
      Origin: "https://untrusted.invalid",
      "Access-Control-Request-Method": "GET",
      "Access-Control-Request-Headers": requestHeaders.join(","),
    },
  });
  assert.equal(untrusted.headers.get("access-control-allow-origin"), null, "An untrusted origin cannot read credentialed responses");
}

/** Exercise domain selection through the actual backend and auth cookie writers. */
export async function verifyCookieDomains(apiOrigin: string, authOrigin: string, webOrigin: string): Promise<void> {
  for (const [host, domain] of [
    ["FLASHCARDS.VICHR.ME.:443", "flashcards.vichr.me"],
    ["api.flashcards.vichr.me:443", "flashcards.vichr.me"],
    ["vichr-rbpi5.tailb8724c.ts.net:10443", "vichr-rbpi5.tailb8724c.ts.net"],
    ["LOCALHOST.:443", "localhost"],
    ["api.localhost:19400", "localhost"],
    ["auth.localhost:19401", "auth.localhost"],
  ]) {
    assert.ok(host && domain);
    for (const granted of [true, false]) {
      const visitor = await postWithHost(`${apiOrigin}/v1/analytics/visitor`, webOrigin, host, JSON.stringify({ granted }));
      assert.equal(visitor.status, 200);
      const cookies = visitor.cookies;
      assert.equal(cookies.length, 1);
      assert.ok(cookies[0]?.includes(`Domain=${domain};`), `Visitor cookies use the matching domain ${domain}`);
      if (!granted) assert.ok(cookies[0]?.includes("Max-Age=0"));
    }
    const refresh = await postWithHost(`${authOrigin}/api/refresh-session`, webOrigin, host);
    assert.equal(refresh.status, 401, "An absent session cannot refresh");
    const cookies = refresh.cookies;
    assert.equal(cookies.length, 3);
    for (const cookie of cookies) {
      assert.ok(cookie.includes(`Domain=${domain}`), `Session deletions use the matching domain ${domain}`);
      assert.ok(cookie.includes("Max-Age=0"));
    }
  }
  for (const host of ["evillocalhost:19401", "evilflashcards.vichr.me", "flashcards.vichr.me.attacker.invalid"]) {
    for (const [url, body] of [
      [`${apiOrigin}/v1/analytics/visitor`, JSON.stringify({ granted: true })],
      [`${authOrigin}/api/refresh-session`, undefined],
    ]) {
      assert.ok(url);
      const response = await postWithHost(url, webOrigin, host, body);
      assert.ok(response.status >= 500, "An unrelated host cannot receive a fallback cookie domain");
      assert.equal(response.cookies.length, 0);
    }
  }
  const page = await fetch(`${authOrigin}/login?redirect_uri=${encodeURIComponent(webOrigin)}`);
  assert.equal(page.status, 200);
  const loginCookie = page.headers.getSetCookie().find(cookie => cookie.startsWith("local_login_csrf="));
  assert.ok(loginCookie?.includes("HttpOnly") && loginCookie.includes("SameSite=Strict"));
  assert.ok(!loginCookie.includes("Domain="), "The login CSRF cookie remains host-only");
}
