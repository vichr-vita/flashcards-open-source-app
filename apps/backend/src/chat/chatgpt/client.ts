import OpenAI from "openai";
import { z } from "zod";
import { HttpError } from "../../shared/errors";
import { codexBaseUrl, getChatGPTCredentials, type ChatGPTReference } from "./connection";

const requestSchema = z.object({
  input: z.array(z.object({ type: z.string().optional(), role: z.string().optional(), content: z.unknown().optional() }).passthrough()),
}).passthrough();

/** Keep the existing Responses tool loop, adapting its requests to Codex's subscription endpoint. */
export function createChatGPTClient(userId: string, reference: ChatGPTReference): OpenAI {
  return new OpenAI({
    // The SDK requires a key. The transport replaces it with a fresh server-owned OAuth token.
    apiKey: "server-owned-chatgpt-connection",
    baseURL: codexBaseUrl,
    maxRetries: 0,
    fetch: async (input, init) => {
      if (String(input) !== `${codexBaseUrl}/responses` || typeof init?.body !== "string") {
        throw new HttpError(400, "This ChatGPT connection supports chat responses only.", "CHATGPT_OPERATION_UNSUPPORTED");
      }
      const original = requestSchema.parse(JSON.parse(init.body));
      const instructions: string[] = [];
      const messages = original.input.filter((item) => {
        if (item.role !== "system" && item.role !== "developer") return true;
        if (typeof item.content === "string") instructions.push(item.content);
        else {
          const content = z.array(z.object({ text: z.string().optional() }).passthrough()).parse(item.content);
          for (const part of content) if (part.text !== undefined) instructions.push(part.text);
        }
        return false;
      });
      const { max_output_tokens: _maxTokens, safety_identifier: _safetyIdentifier, ...body } = original;
      const adaptedBody = JSON.stringify({ ...body, model: reference.modelId, instructions: instructions.join("\n\n"), input: messages, store: false, stream: true });
      async function send(forceRefresh: boolean): Promise<Response> {
        const credentials = await getChatGPTCredentials(userId, reference, forceRefresh);
        const headers = new Headers(init?.headers);
        credentials.forEach((value, name) => headers.set(name, value));
        headers.set("Accept", "text/event-stream");
        try {
          return await fetch(`${codexBaseUrl}/responses`, { ...init, headers, body: adaptedBody, redirect: "error" });
        } catch (error) {
          if (init?.signal?.aborted) throw error;
          throw new HttpError(502, "Cannot reach ChatGPT. Try again.", "CHATGPT_UNAVAILABLE");
        }
      }
      let response = await send(false);
      if (response.status === 401) { await response.body?.cancel(); response = await send(true); }
      if (response.ok) return response;
      await response.body?.cancel();
      const message = response.status === 429
        ? "Your ChatGPT usage limit was reached. Try later or select API in AI settings."
        : response.status === 401 || response.status === 403
          ? "ChatGPT needs authorization. Connect again in AI settings."
          : "ChatGPT could not complete this request. Try again or check AI settings.";
      // Provider bodies can contain credential details. Only a reviewed message reaches logs/UI.
      return new Response(JSON.stringify({ error: { message, type: "chatgpt_error", code: "CHATGPT_REQUEST_FAILED" } }), { status: response.status, headers: { "Content-Type": "application/json" } });
    },
  });
}
