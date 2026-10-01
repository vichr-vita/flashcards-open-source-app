import { useId, useState, type ReactElement } from "react";
import { Link } from "react-router";
import { changeAISettings } from "../../api/endpoints/aiSettings";
import { isAuthRedirectError } from "../../api";
import { useAISettings } from "../../chat/preferences/useAISettings";
import { useI18n } from "../../i18n";
import { useAppData } from "../../appData";
import { buildWorkspaceRoute, settingsOwnOpenAIKeyRoute, settingsHubRoute } from "../../routes";

export function AISettingsScreen(): ReactElement {
  const { t } = useI18n();
  const { activeWorkspace } = useAppData();
  const { settings, error: loadError } = useAISettings();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const modelFieldId = useId();
  const effortFieldId = useId();
  const workspaceId = activeWorkspace?.workspaceId;
  async function act(action: Parameters<typeof changeAISettings>[0], modelId?: string, reasoningEffort?: string): Promise<void> {
    setBusy(true); setError(null); setCopied(false);
    try { await changeAISettings(action, modelId, reasoningEffort); }
    catch (caught) { if (!isAuthRedirectError(caught)) setError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setBusy(false); }
  }
  const connection = settings?.connection;
  const supportedEfforts = connection?.models.find((model) => model.id === connection.modelId)?.supportedReasoningEfforts ?? [];
  function effortLabel(effort: string): string {
    switch (effort) {
      case "none": return t("aiSettings.efforts.none");
      case "minimal": return t("aiSettings.efforts.minimal");
      case "low": return t("aiSettings.efforts.low");
      case "medium": return t("aiSettings.efforts.medium");
      case "high": return t("aiSettings.efforts.high");
      case "xhigh": return t("aiSettings.efforts.xhigh");
      case "max": return t("aiSettings.efforts.max");
      case "ultra": return t("aiSettings.efforts.ultra");
      case "persistent": return t("aiSettings.efforts.persistent");
      default: return effort;
    }
  }
  const login = settings?.login;
  return (
    <main className="container ai-settings-page">
      <Link to={buildWorkspaceRoute(workspaceId ?? "", settingsHubRoute)}>{t("settingsHome.title")}</Link>
      <h1>{t("aiSettings.title")}</h1>
      {(error ?? loadError ?? settings?.error) ? <p role="alert" className="error-banner">{error ?? loadError ?? settings?.error}</p> : null}
      {settings === null ? (
        <p role="status">{loadError === null ? t("common.loading") : <button type="button" onClick={() => window.dispatchEvent(new Event("ai-settings-changed"))}>{t("common.retry")}</button>}</p>
      ) : (
        <>
          <fieldset disabled={busy || !settings.enabled} className="ai-provider-selector">
            <legend>{t("aiSettings.provider")}</legend>
            <label><input type="radio" name="ai-provider" checked={settings.provider === "chatgpt"} disabled={connection == null} onChange={() => void act("chatgpt")} />ChatGPT</label>
            <label><input type="radio" name="ai-provider" checked={settings.provider === "api"} onChange={() => void act("api")} />OpenAI API</label>
          </fieldset>
          <section aria-labelledby="chatgpt-settings-heading">
            <h2 id="chatgpt-settings-heading">ChatGPT</h2>
            {!settings.enabled ? <p>{t("aiSettings.notConfigured")}</p> : connection == null ? <p>{t("aiSettings.notConnected")}</p> : (
              <>
                <p>{connection.email ?? "ChatGPT"}{connection.plan === null ? "" : ` · ${connection.plan}`}</p>
                <label htmlFor={modelFieldId}>{t("aiSettings.model")}</label>
                <select id={modelFieldId} value={connection.modelId} disabled={busy} onChange={(event) => void act("chatgpt", event.target.value)}>
                  {connection.models.map((model) => <option key={model.id} value={model.id}>{model.name}</option>)}
                </select>
                <label htmlFor={effortFieldId}>{t("aiSettings.reasoningEffort")}</label>
                <select id={effortFieldId} value={connection.reasoningEffort ?? ""} disabled={busy || supportedEfforts.length === 0} aria-describedby={`${effortFieldId}-hint`} onChange={(event) => void act("chatgpt", undefined, event.target.value)}>
                  {supportedEfforts.length === 0 ? <option value="">{t("common.unavailable")}</option> : supportedEfforts.map((effort) => <option key={effort} value={effort}>{effortLabel(effort)}</option>)}
                </select>
                <p id={`${effortFieldId}-hint`} className="ai-reasoning-effort-hint">{t("aiSettings.reasoningEffortHint")}</p>
              </>
            )}
            <p>{t("aiSettings.supported")}</p>
            {login == null ? (
              <div className="ai-settings-actions">
                <button type="button" disabled={busy || !settings.enabled} onClick={() => void act("start")}>{t("aiSettings.connect")}</button>
                {connection == null ? null : <button type="button" disabled={busy} onClick={() => void act("disconnect")}>{t("aiSettings.disconnect")}</button>}
              </div>
            ) : (
              <div className="ai-device-login">
                <p>{t("aiSettings.codeInstruction")}</p>
                <code dir="ltr" className="ai-device-code">{login.userCode}</code>
                <div className="ai-settings-actions">
                  <button type="button" onClick={() => { void navigator.clipboard.writeText(login.userCode).then(() => setCopied(true)).catch(() => setError(t("aiSettings.copyFailed"))); }}>{copied ? t("aiSettings.copied") : t("aiSettings.copyCode")}</button>
                  <a href="https://auth.openai.com/codex/device" target="_blank" rel="noopener noreferrer">{t("aiSettings.openSignIn")}</a>
                  <button type="button" disabled={busy} onClick={() => void act("cancel")}>{t("common.cancel")}</button>
                </div>
                <p role="status">{t("aiSettings.waiting")}</p>
              </div>
            )}
          </section>
          <section aria-labelledby="api-settings-heading">
            <h2 id="api-settings-heading">OpenAI API</h2>
            <p>{t("aiSettings.apiDescription")}</p>
            <Link to={buildWorkspaceRoute(workspaceId ?? "", settingsOwnOpenAIKeyRoute)}>{t("ownOpenAIKeySettings.title")}</Link>
          </section>
          {busy ? <p role="status">{t("common.loading")}</p> : null}
        </>
      )}
    </main>
  );
}
