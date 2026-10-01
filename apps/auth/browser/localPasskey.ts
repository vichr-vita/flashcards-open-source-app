import { browserSupportsWebAuthn, startAuthentication, startRegistration, type PublicKeyCredentialCreationOptionsJSON, type PublicKeyCredentialRequestOptionsJSON } from "@simplewebauthn/browser";

const configNode = document.getElementById("local-auth-config");
const button = document.getElementById("passkey");
const title = document.getElementById("title");
const hint = document.getElementById("hint");
const feedback = document.getElementById("feedback");
if (!configNode || !(button instanceof HTMLButtonElement) || !title || !hint || !feedback) throw new Error("Missing sign-in controls");
const config = JSON.parse(configNode.textContent ?? "{}") as { csrfToken: string; redirectUri: string; enrollment: boolean };
let enrollment = config.enrollment;
// The grant remains in memory only. Remove it before any network request or user interaction.
let grant = enrollment ? new URLSearchParams(location.hash.slice(1)).get("enroll") ?? "" : "";
if (location.hash) history.replaceState(null, "", location.pathname + location.search);
// Reopening a setup link in this tab can be a fragment-only navigation.
if (config.enrollment) window.addEventListener("hashchange", () => { if (location.hash) location.reload(); });

class RequestError extends Error { constructor(message: string, readonly code: string | undefined) { super(message); } }
async function post<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(`/api/webauthn/${path}`, { method: "POST", credentials: "same-origin", cache: "no-store", headers: { "Content-Type": "application/json", "X-CSRF-Token": config.csrfToken }, body: JSON.stringify(body) });
  const result: unknown = await response.json();
  if (!response.ok) {
    const error = result as { error?: string; code?: string };
    throw new RequestError(error.error ?? "Sign-in failed. Try again.", error.code);
  }
  return result as T;
}

if (!browserSupportsWebAuthn()) {
  button.disabled = true;
  feedback.textContent = "Passkeys are unavailable in this browser.";
} else if (enrollment && !/^[A-Za-z0-9_-]{43}$/.test(grant)) {
  button.disabled = true;
  feedback.textContent = "This link has expired. Use a new setup link.";
} else {
  button.addEventListener("click", async () => {
    button.disabled = true;
    feedback.textContent = "";
    try {
      if (enrollment) {
        const optionsJSON = await post<PublicKeyCredentialCreationOptionsJSON>("registration/options", { grant });
        const credential = await startRegistration({ optionsJSON });
        await post("registration/verify", { grant, credential });
        grant = "";
        enrollment = false;
        title.textContent = "Sign in";
        button.textContent = "Sign in with passkey";
        hint.textContent = "Passkey saved.";
      } else {
        const optionsJSON = await post<PublicKeyCredentialRequestOptionsJSON>("authentication/options", {});
        const credential = await startAuthentication({ optionsJSON });
        await post("authentication/verify", credential);
        location.replace(config.redirectUri);
      }
    } catch (error: unknown) {
      const cause = error instanceof Error ? error.cause : undefined;
      const name = cause instanceof Error || cause instanceof DOMException ? cause.name : error instanceof Error ? error.name : "";
      const cancelled = name === "NotAllowedError" || name === "AbortError";
      feedback.textContent = cancelled ? (enrollment ? "Passkey setup cancelled. Try again." : "Sign-in cancelled. Try again.") : error instanceof RequestError ? error.message : enrollment ? "Passkey setup failed. Try again." : "Sign-in failed. Try again.";
      if (error instanceof RequestError && error.code === "ENROLLMENT_INVALID") { grant = ""; return; }
    } finally {
      button.disabled = enrollment && grant === "";
    }
  });
}
