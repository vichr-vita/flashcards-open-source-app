import { parseArray, parseBoolean, parseEnum, parseNullableString, parseNumber, parseObject, parseString } from "../../apiContracts/core";
import { ApiError } from "../transport/errors";
import { parseContractResponse } from "../transport/response";
import { allowAuthRecovery, requestJson } from "../transport/transport";

function parseAISettings(value: unknown, endpoint: string) {
  const body = parseObject(value, endpoint, "");
  const connection = body.connection === null ? null : parseObject(body.connection, endpoint, "connection");
  const login = body.login === null ? null : parseObject(body.login, endpoint, "login");
  return {
    enabled: parseBoolean(body.enabled, endpoint, "enabled"),
    provider: parseEnum(body.provider, endpoint, "provider", ["api", "chatgpt"] as const),
    connection: connection === null ? null : {
      email: parseNullableString(connection.email, endpoint, "connection.email"),
      plan: parseNullableString(connection.plan, endpoint, "connection.plan"),
      modelId: parseString(connection.modelId, endpoint, "connection.modelId"),
      models: parseArray(connection.models, endpoint, "connection.models", (item, pathEndpoint, path) => {
        const model = parseObject(item, pathEndpoint, path);
        return { id: parseString(model.id, pathEndpoint, `${path}.id`), name: parseString(model.name, pathEndpoint, `${path}.name`) };
      }),
    },
    login: login === null ? null : {
      verificationUrl: parseString(login.verificationUrl, endpoint, "login.verificationUrl"),
      userCode: parseString(login.userCode, endpoint, "login.userCode"),
      expiresAt: parseNumber(login.expiresAt, endpoint, "login.expiresAt"),
    },
    error: parseNullableString(body.error, endpoint, "error"),
  };
}
export type AISettings = ReturnType<typeof parseAISettings>;

export async function loadAISettings(signal?: AbortSignal): Promise<AISettings | null> {
  try { return parseContractResponse(await requestJson("/ai/settings", { method: "GET", signal }, allowAuthRecovery), "GET /ai/settings", parseAISettings); }
  catch (error) { if (error instanceof ApiError && error.statusCode === 404) return null; throw error; }
}

export async function changeAISettings(action: "start" | "cancel" | "disconnect" | "api" | "chatgpt", modelId?: string): Promise<AISettings> {
  const path = action === "start" ? "/ai/settings/chatgpt/start" : "/ai/settings";
  const settings = parseContractResponse(await requestJson(path, { method: "POST", body: JSON.stringify(action === "start" ? {} : { action, modelId }) }, allowAuthRecovery), `POST ${path}`, parseAISettings);
  window.dispatchEvent(new Event("ai-settings-changed"));
  return settings;
}
