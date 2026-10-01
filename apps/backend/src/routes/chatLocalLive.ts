import { PassThrough, Readable } from "node:stream";
import { Hono } from "hono";
import { runLiveStream } from "../chat/live";
import { handleLiveRequest } from "../chat/live/request";
import { loadRequestContextFromRequest } from "../server/requestContext";
import type { AppEnv } from "../server/appEnv";
import { HttpError } from "../shared/errors";

/** Local Node serves the same persisted SSE overlay as the AWS live-stream function. */
export function createChatLocalLiveRoutes(options: Readonly<{ allowedOrigins: string[] }>): Hono<AppEnv> {
  const app = new Hono<AppEnv>();
  app.get("/chat/live", async (context) => {
    // The signed Live envelope scopes the run; a current cookie also enforces logout/revocation.
    const headers = new Headers(context.req.raw.headers);
    headers.delete("Authorization");
    const sessionRequest = new Request(context.req.raw, { headers });
    const { requestContext } = await loadRequestContextFromRequest(sessionRequest, options.allowedOrigins);
    const params = await handleLiveRequest(new URL(context.req.url), context.req.header("Authorization"), context.req.raw.headers);
    if (params.userId !== requestContext.userId || requestContext.transport !== "session") throw new HttpError(403, "This chat belongs to another account.", "CHAT_LIVE_AUTH_INVALID");
    const stream = new PassThrough();
    const abort = (): void => { stream.destroy(); };
    context.req.raw.signal.addEventListener("abort", abort, { once: true });
    const response = new Response(Readable.toWeb(stream) as ReadableStream<Uint8Array>, { headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-store", "X-Accel-Buffering": "no" } });
    void runLiveStream(stream, { ...params, requestId: context.get("requestId") }).catch(() => { stream.destroy(); }).finally(() => { context.req.raw.signal.removeEventListener("abort", abort); });
    return response;
  });
  return app;
}
