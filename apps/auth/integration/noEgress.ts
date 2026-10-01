/** Imported only by the HTTP integration children, so unexpected AWS/email calls fail the run. */
import http from "node:http";
import https from "node:https";

function requireLoopback(hostname: string): void {
  if (hostname !== "localhost" && hostname !== "127.0.0.1" && hostname !== "::1" && hostname !== "[::1]") {
    console.error("Unexpected outbound network request blocked");
    throw new Error("Unexpected outbound network request");
  }
}

const originalFetch = globalThis.fetch;
globalThis.fetch = (input, init) => {
  const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
  if (process.env.LOCAL_CHATGPT_FIXTURE === "true" && (url.origin === "https://auth.openai.com" || url.origin === "https://chatgpt.com")) {
    return originalFetch(`http://127.0.0.1:19402${url.pathname}${url.search}`, init);
  }
  requireLoopback(url.hostname);
  return originalFetch(input, init);
};

for (const transport of [http, https]) {
  for (const method of ["request", "get"] as const) {
    transport[method] = new Proxy(transport[method], {
      apply(target, thisArg: unknown, args: Array<unknown>) {
        const input = args[0];
        if (typeof input === "string" || input instanceof URL) requireLoopback(new URL(input).hostname);
        else if (input && typeof input === "object") {
          const hostname = "hostname" in input ? input.hostname : "host" in input ? input.host : "localhost";
          requireLoopback(String(hostname));
        }
        return Reflect.apply(target, thisArg, args);
      },
    });
  }
}
