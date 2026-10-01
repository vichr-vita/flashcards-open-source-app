import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile, lstat, unlink } from "node:fs/promises";
import { constants } from "node:fs";
import { isAbsolute, join } from "node:path";
import { z } from "zod";
import { getAuthConfig } from "../../auth/config";
import { HttpError } from "../../shared/errors";

// Public client and protocol used by openai/codex's device-code login.
const clientId = "app_EMoamEEZ73f0CkXaXp7hrann";
const issuer = "https://auth.openai.com";
export const codexBaseUrl = "https://chatgpt.com/backend-api/codex";
const verificationUrl = "https://auth.openai.com/codex/device";
const tokenSchema = z.object({ access_token: z.string().min(1), refresh_token: z.string().min(1), id_token: z.string().min(1) });
export const chatGPTReasoningEffortSchema = z.string().min(1).max(100);
const modelSchema = z.object({
  id: z.string().min(1), name: z.string().min(1),
  supportedReasoningEfforts: z.array(chatGPTReasoningEffortSchema).optional(),
  defaultReasoningEffort: chatGPTReasoningEffortSchema.nullable().optional(),
});
const connectionSchema = z.object({
  id: z.string().uuid(), accountId: z.string().min(1), email: z.string().nullable(), plan: z.string().nullable(),
  idToken: z.string().min(1), accessToken: z.string().min(1), refreshToken: z.string().min(1), expiresAt: z.number(),
  models: z.array(modelSchema), modelId: z.string().min(1),
  reasoningEffort: chatGPTReasoningEffortSchema.nullable().optional(),
});
const stateSchema = z.object({ userId: z.string().uuid(), provider: z.enum(["api", "chatgpt"]), connection: connectionSchema.nullable() });
type State = z.infer<typeof stateSchema>;
type Connection = z.infer<typeof connectionSchema>;
export type ChatGPTReference = Readonly<{ connectionId: string; modelId: string; reasoningEffort: string | null }>;
type Pending = { userId: string; deviceId: string; userCode: string; expiresAt: number; nextPollAt: number; intervalMs: number };
let pending: Pending | null = null;
let loginError: string | null = null;
let lock: Promise<void> = Promise.resolve();

// One backend process owns the private connection. Serialize refresh, polling and disconnect so
// an in-flight token response cannot restore credentials after the user disconnects.
async function serialized<T>(operation: () => Promise<T>): Promise<T> {
  const previous = lock;
  let release = (): void => {};
  lock = new Promise<void>((resolve) => { release = resolve; });
  await previous;
  try { return await operation(); } finally { release(); }
}

export function isChatGPTConfigured(): boolean {
  return process.env.AUTH_MODE === "local" && Boolean(process.env.CHATGPT_CONNECTION_DIR) && getAuthConfig().mode === "local";
}

function statePath(): string {
  const directory = process.env.CHATGPT_CONNECTION_DIR;
  if (!isChatGPTConfigured() || directory === undefined || !isAbsolute(directory)) {
    throw new HttpError(409, "Set CHATGPT_CONNECTION_DIR to a private absolute directory on this server.", "CHATGPT_NOT_CONFIGURED");
  }
  return join(directory, "connection.json");
}

async function readState(userId: string): Promise<State> {
  const path = statePath();
  let contents: string;
  try {
    const file = await lstat(path);
    if (!file.isFile() || (file.mode & 0o077) !== 0 || file.uid !== process.getuid?.() || file.size > 1024 * 1024) throw new Error("Invalid credential file permissions");
    contents = await readFile(path, { encoding: "utf8", flag: constants.O_RDONLY | constants.O_NOFOLLOW });
  }
  catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") return { userId, provider: "api", connection: null };
    throw new HttpError(503, "Cannot read the private ChatGPT connection file.", "CHATGPT_STORAGE_UNAVAILABLE");
  }
  let parsed;
  try { parsed = stateSchema.safeParse(JSON.parse(contents)); }
  catch { throw new HttpError(503, "The private ChatGPT connection file is invalid.", "CHATGPT_STORAGE_INVALID"); }
  if (!parsed.success) throw new HttpError(503, "The private ChatGPT connection file is invalid.", "CHATGPT_STORAGE_INVALID");
  if (parsed.data.userId !== userId) throw new HttpError(403, "This server's ChatGPT connection belongs to another account.", "CHATGPT_OWNER_MISMATCH");
  return parsed.data;
}

/** Upgrade old model catalogs only when using ChatGPT, so reconnect/disconnect/API remain available. */
async function upgradeConnectionModels(state: State): Promise<State> {
  const connection = state.connection;
  if (connection !== null && (connection.reasoningEffort === undefined || connection.models.some((model) => model.supportedReasoningEfforts === undefined))) {
    const refreshed = await refreshConnection(connection);
    // Preserve rotated refresh tokens even if the subsequent catalog request fails.
    if (refreshed !== connection) await saveState({ ...state, connection: refreshed });
    const upgraded = { ...state, connection: await loadModels(refreshed) };
    await saveState(upgraded);
    return upgraded;
  }
  return state;
}

async function saveState(state: State): Promise<void> {
  const path = statePath();
  const directory = process.env.CHATGPT_CONNECTION_DIR;
  if (directory === undefined) throw new Error("ChatGPT connection directory is missing");
  const temporary = `${path}.${randomUUID()}.tmp`;
  try {
    await mkdir(directory, { recursive: true, mode: 0o700 });
    const folder = await lstat(directory);
    if (!folder.isDirectory() || (folder.mode & 0o077) !== 0 || folder.uid !== process.getuid?.()) throw new Error("Invalid credential directory permissions");
    await writeFile(temporary, JSON.stringify(state), { mode: 0o600, flag: "wx" });
    await rename(temporary, path);
  } catch {
    await unlink(temporary).catch(() => {});
    throw new HttpError(503, "Cannot save the private ChatGPT connection file.", "CHATGPT_STORAGE_UNAVAILABLE");
  }
}

async function authPost(path: string, body: Record<string, string>, form = false): Promise<Response> {
  try {
    return await fetch(`${issuer}${path}`, { method: "POST", headers: { "Content-Type": form ? "application/x-www-form-urlencoded" : "application/json" }, body: form ? new URLSearchParams(body).toString() : JSON.stringify(body), signal: AbortSignal.timeout(15_000), redirect: "error" });
  } catch {
    throw new HttpError(502, "Cannot reach ChatGPT sign-in. Try again.", "CHATGPT_AUTH_UNAVAILABLE");
  }
}

async function readProviderJSON(response: Response): Promise<unknown> {
  try { return await response.json(); }
  catch { throw new HttpError(502, "ChatGPT returned an invalid response. Try again.", "CHATGPT_RESPONSE_INVALID"); }
}

function claims(token: string): Record<string, unknown> {
  try { return z.record(z.string(), z.unknown()).parse(JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString("utf8"))); }
  catch { throw new HttpError(502, "ChatGPT returned an invalid token.", "CHATGPT_TOKEN_INVALID"); }
}

function tokensToConnection(tokens: z.infer<typeof tokenSchema>, previous?: Connection): Connection {
  // These tokens only come from the fixed HTTPS token endpoint, never from browser input.
  // Claims supply display data, account routing and refresh timing, not local authorization.
  const id = claims(tokens.id_token);
  const auth = z.object({ chatgpt_account_id: z.string().min(1), chatgpt_plan_type: z.string().optional(), chatgpt_account_is_fedramp: z.boolean().optional() }).safeParse(id["https://api.openai.com/auth"]);
  const exp = z.number().positive().safeParse(claims(tokens.access_token).exp);
  if (!auth.success || !exp.success || auth.data.chatgpt_account_is_fedramp === true) throw new HttpError(502, "ChatGPT returned an unsupported account token.", "CHATGPT_TOKEN_INVALID");
  if (previous !== undefined && auth.data.chatgpt_account_id !== previous.accountId) throw new HttpError(401, "The ChatGPT account changed. Connect again in AI settings.", "CHATGPT_RECONNECT_REQUIRED");
  const profile = z.object({ email: z.string().optional() }).safeParse(id["https://api.openai.com/profile"]);
  return {
    id: previous?.id ?? randomUUID(), accountId: auth.data.chatgpt_account_id,
    email: typeof id.email === "string" ? id.email : profile.success ? profile.data.email ?? null : null,
    plan: auth.data.chatgpt_plan_type ?? null, idToken: tokens.id_token, accessToken: tokens.access_token, refreshToken: tokens.refresh_token,
    expiresAt: exp.data * 1000, models: previous?.models ?? [], modelId: previous?.modelId ?? "",
    reasoningEffort: previous?.reasoningEffort ?? null,
  };
}

export function chatGPTHeaders(connection: Pick<Connection, "accessToken" | "accountId">): Headers {
  return new Headers({ Authorization: `Bearer ${connection.accessToken}`, "ChatGPT-Account-Id": connection.accountId, originator: "codex_cli_rs" });
}

async function loadModels(connection: Connection): Promise<Connection> {
  let response: Response;
  try { response = await fetch(`${codexBaseUrl}/models?client_version=0.159.3`, { headers: chatGPTHeaders(connection), signal: AbortSignal.timeout(15_000), redirect: "error" }); }
  catch { throw new HttpError(502, "Cannot load ChatGPT models. Try connecting again.", "CHATGPT_MODELS_UNAVAILABLE"); }
  if (!response.ok) throw new HttpError(502, "Cannot load models for this ChatGPT account. Try connecting again.", "CHATGPT_MODELS_UNAVAILABLE");
  const catalog = z.object({ models: z.array(z.object({
    slug: z.string(), display_name: z.string(), visibility: z.string().optional(),
    default_reasoning_level: chatGPTReasoningEffortSchema.nullish(),
    supported_reasoning_levels: z.array(z.object({ effort: chatGPTReasoningEffortSchema })).default([]),
  })) }).safeParse(await readProviderJSON(response));
  if (!catalog.success) throw new HttpError(502, "ChatGPT returned an invalid model list.", "CHATGPT_MODELS_UNAVAILABLE");
  const models = catalog.data.models.filter((model) => model.visibility === undefined || model.visibility === "list").map((model) => {
    const supportedReasoningEfforts = model.supported_reasoning_levels.map((level) => level.effort);
    const defaultReasoningEffort = model.default_reasoning_level != null && supportedReasoningEfforts.includes(model.default_reasoning_level)
      ? model.default_reasoning_level : supportedReasoningEfforts[0] ?? null;
    return { id: model.slug, name: model.display_name, supportedReasoningEfforts, defaultReasoningEffort };
  });
  const first = models[0];
  if (first === undefined) throw new HttpError(409, "No chat models are available for this ChatGPT account.", "CHATGPT_MODELS_UNAVAILABLE");
  const selected = models.find((model) => model.id === connection.modelId) ?? first;
  const reasoningEffort = connection.reasoningEffort != null && selected.supportedReasoningEfforts.includes(connection.reasoningEffort)
    ? connection.reasoningEffort : selected.defaultReasoningEffort;
  return { ...connection, models, modelId: selected.id, reasoningEffort };
}

async function pollLogin(userId: string): Promise<void> {
  const attempt = pending;
  if (attempt === null || attempt.userId !== userId) return;
  if (Date.now() >= attempt.expiresAt) { pending = null; loginError = "Sign-in expired. Start again."; return; }
  if (Date.now() < attempt.nextPollAt) return;
  attempt.nextPollAt = Date.now() + attempt.intervalMs;
  try {
    const response = await authPost("/api/accounts/deviceauth/token", { device_auth_id: attempt.deviceId, user_code: attempt.userCode });
    if (response.status === 403 || response.status === 404) return;
    if (!response.ok) throw new HttpError(502, "ChatGPT sign-in failed. Start again.", "CHATGPT_LOGIN_FAILED");
    const code = z.object({ authorization_code: z.string(), code_verifier: z.string() }).parse(await readProviderJSON(response));
    // The authorization code is single-use. Never retry its exchange automatically.
    const exchanged = await authPost("/oauth/token", { grant_type: "authorization_code", client_id: clientId, code: code.authorization_code, code_verifier: code.code_verifier, redirect_uri: `${issuer}/deviceauth/callback` }, true);
    if (!exchanged.ok) throw new HttpError(502, "ChatGPT sign-in failed. Start again.", "CHATGPT_LOGIN_FAILED");
    const connection = await loadModels(tokensToConnection(tokenSchema.parse(await readProviderJSON(exchanged))));
    const state = await readState(userId);
    await saveState({ ...state, provider: "chatgpt", connection });
    pending = null; loginError = null;
  } catch (error) {
    pending = null;
    loginError = error instanceof HttpError ? error.message : "ChatGPT sign-in failed. Start again.";
  }
}

export async function getAISettings(userId: string) {
  if (!isChatGPTConfigured()) return { enabled: false, provider: "api" as const, connection: null, login: null, error: null };
  return serialized(async () => {
    await pollLogin(userId);
    const savedState = await readState(userId);
    const state = savedState.provider === "chatgpt" ? await upgradeConnectionModels(savedState) : savedState;
    const connection = state.connection;
    return {
      enabled: true, provider: state.provider,
      connection: connection === null ? null : { email: connection.email, plan: connection.plan, modelId: connection.modelId, reasoningEffort: connection.reasoningEffort ?? null, models: connection.models },
      login: pending?.userId === userId ? { verificationUrl, userCode: pending.userCode, expiresAt: pending.expiresAt } : null,
      error: loginError,
    };
  });
}

export async function startChatGPTLogin(userId: string): Promise<void> {
  await serialized(async () => {
    await readState(userId);
    if (pending?.userId === userId && pending.expiresAt > Date.now()) return;
    const response = await authPost("/api/accounts/deviceauth/usercode", { client_id: clientId });
    if (!response.ok) throw new HttpError(502, "Enable device-code login in ChatGPT security settings, then try again.", "CHATGPT_LOGIN_FAILED");
    const code = z.object({ device_auth_id: z.string(), user_code: z.string().optional(), usercode: z.string().optional(), interval: z.union([z.string(), z.number()]).optional() }).safeParse(await readProviderJSON(response));
    const userCode = code.success ? code.data.user_code ?? code.data.usercode : undefined;
    if (!code.success || userCode === undefined) throw new HttpError(502, "ChatGPT returned an invalid sign-in code.", "CHATGPT_LOGIN_FAILED");
    const intervalMs = Math.max(1000, Math.min(60_000, Number(code.data.interval ?? 5) * 1000));
    pending = { userId, deviceId: code.data.device_auth_id, userCode, expiresAt: Date.now() + 900_000, intervalMs: Number.isFinite(intervalMs) ? intervalMs : 5000, nextPollAt: Date.now() + 1000 };
    loginError = null;
  });
}

export async function updateAISettings(userId: string, action: "cancel" | "disconnect" | "api" | "chatgpt", modelId?: string, reasoningEffort?: string): Promise<void> {
  await serialized(async () => {
    const savedState = await readState(userId);
    const state = action === "chatgpt" ? await upgradeConnectionModels(savedState) : savedState;
    if (action === "cancel" || action === "disconnect") { pending = null; loginError = null; }
    if (action === "disconnect") await saveState({ ...state, connection: null });
    if (action === "api") await saveState({ ...state, provider: "api" });
    if (action === "chatgpt") {
      const connection = state.connection;
      if (connection === null) throw new HttpError(409, "Connect ChatGPT first.", "CHATGPT_RECONNECT_REQUIRED");
      const selected = connection.models.find((model) => model.id === (modelId ?? connection.modelId));
      if (selected === undefined) throw new HttpError(400, "Select an available ChatGPT model.", "CHATGPT_MODEL_INVALID");
      const supported = selected.supportedReasoningEfforts ?? [];
      if (reasoningEffort !== undefined && !supported.includes(reasoningEffort)) {
        throw new HttpError(400, "Select a reasoning effort supported by this ChatGPT model.", "CHATGPT_REASONING_EFFORT_INVALID");
      }
      const effort = reasoningEffort ?? (connection.reasoningEffort != null && supported.includes(connection.reasoningEffort)
        ? connection.reasoningEffort : selected.defaultReasoningEffort ?? null);
      await saveState({ ...state, provider: "chatgpt", connection: { ...connection, modelId: selected.id, reasoningEffort: effort } });
    }
  });
}

export async function resolveChatGPTReference(userId: string): Promise<ChatGPTReference | null> {
  if (!isChatGPTConfigured()) return null;
  return serialized(async () => {
    const savedState = await readState(userId);
    if (savedState.provider === "api") return null;
    const state = await upgradeConnectionModels(savedState);
    if (state.connection === null) throw new HttpError(409, "Reconnect ChatGPT or select API in AI settings.", "CHATGPT_RECONNECT_REQUIRED");
    return { connectionId: state.connection.id, modelId: state.connection.modelId, reasoningEffort: state.connection.reasoningEffort ?? null };
  });
}

/** Refresh tokens without reacquiring the connection lock, including during a legacy catalog upgrade. */
async function refreshConnection(connection: Connection, forceRefresh = false): Promise<Connection> {
  if (forceRefresh || connection.expiresAt < Date.now() + 60_000) {
    const response = await authPost("/oauth/token", { grant_type: "refresh_token", client_id: clientId, refresh_token: connection.refreshToken });
    if (!response.ok) throw new HttpError(401, "ChatGPT sign-in expired. Connect again in AI settings.", "CHATGPT_RECONNECT_REQUIRED");
    const responseTokens = z.object({ access_token: z.string().min(1), refresh_token: z.string().min(1).optional(), id_token: z.string().min(1).optional() }).safeParse(await readProviderJSON(response));
    const refreshed = responseTokens.success ? tokenSchema.safeParse({
      access_token: responseTokens.data.access_token,
      refresh_token: responseTokens.data.refresh_token ?? connection.refreshToken,
      id_token: responseTokens.data.id_token ?? connection.idToken,
    }) : responseTokens;
    if (!refreshed.success) throw new HttpError(401, "ChatGPT sign-in expired. Connect again in AI settings.", "CHATGPT_RECONNECT_REQUIRED");
    connection = tokensToConnection(refreshed.data, connection);
  }
  return connection;
}

export async function getChatGPTCredentials(userId: string, reference: ChatGPTReference, forceRefresh = false): Promise<Headers> {
  return serialized(async () => {
    const state = await readState(userId);
    if (state.connection === null || state.connection.id !== reference.connectionId) throw new HttpError(401, "ChatGPT was disconnected. Connect again in AI settings.", "CHATGPT_RECONNECT_REQUIRED");
    const connection = await refreshConnection(state.connection, forceRefresh);
    if (connection !== state.connection) await saveState({ ...state, connection });
    return chatGPTHeaders(connection);
  });
}
