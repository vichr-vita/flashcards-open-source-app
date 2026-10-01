import { Hono } from "hono";
import { z } from "zod";
import { chatGPTReasoningEffortSchema, getAISettings, startChatGPTLogin, updateAISettings } from "../chat/chatgpt/connection";
import { loadRequestContextFromRequest } from "../server/requestContext";
import type { AppEnv } from "../server/appEnv";
import { parseJsonBody } from "../server/requestParsing";
import { HttpError } from "../shared/errors";

export function createAISettingsRoutes(options: Readonly<{ allowedOrigins: string[] }>): Hono<AppEnv> {
  const app = new Hono<AppEnv>();
  async function sessionContext(request: Request) {
    const { requestContext } = await loadRequestContextFromRequest(request, options.allowedOrigins);
    if (requestContext.transport !== "session") throw new HttpError(403, "Sign in to manage AI settings.", "AUTH_UNAUTHORIZED");
    return requestContext;
  }
  app.use("/ai/settings*", async (context, next) => {
    context.header("Cache-Control", "no-store");
    await next();
  });
  app.get("/ai/settings", async (context) => {
    const requestContext = await sessionContext(context.req.raw);
    return context.json(await getAISettings(requestContext.userId));
  });
  app.post("/ai/settings/chatgpt/start", async (context) => {
    const requestContext = await sessionContext(context.req.raw);
    await startChatGPTLogin(requestContext.userId);
    return context.json(await getAISettings(requestContext.userId));
  });
  app.post("/ai/settings", async (context) => {
    const requestContext = await sessionContext(context.req.raw);
    const body = z.object({ action: z.enum(["cancel", "disconnect", "api", "chatgpt"]), modelId: z.string().min(1).max(200).optional(), reasoningEffort: chatGPTReasoningEffortSchema.optional() }).strict().safeParse(await parseJsonBody(context.req.raw));
    if (!body.success) throw new HttpError(400, "Choose an AI provider or connection action.", "AI_SETTINGS_INVALID");
    await updateAISettings(requestContext.userId, body.data.action, body.data.modelId, body.data.reasoningEffort);
    return context.json(await getAISettings(requestContext.userId));
  });
  return app;
}
