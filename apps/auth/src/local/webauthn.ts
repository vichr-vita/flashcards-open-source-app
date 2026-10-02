import {
  generateAuthenticationOptions, generateRegistrationOptions, verifyAuthenticationResponse, verifyRegistrationResponse,
  type AuthenticationResponseJSON, type RegistrationResponseJSON, type AuthenticatorTransport,
} from "@simplewebauthn/server";
import { transaction, type DatabaseExecutor } from "../db.js";
import { getLocalAuthConfig } from "./config.js";
import { hashToken } from "./credentials.js";
import { createSession } from "./store.js";

type Failure = { status: "invalid" } | { status: "throttled"; retryAfter: number };
type Account = { user_id: string; webauthn_user_handle: string; failed_attempts: number; locked_until: Date | null; now: Date };
type Passkey = { credential_id: string; public_key: Buffer; counter: string; transports: AuthenticatorTransport[]; device_type: string };
type Challenge = { grant_hash: string | null; expires_at: Date };
type Ceremony = "authentication" | "registration";

function record(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function encoded(value: unknown, maximum = 16384): value is string { return typeof value === "string" && value.length > 0 && value.length <= maximum && /^[A-Za-z0-9_-]+$/.test(value); }

/** Validate the JSON envelope; the maintained verifier validates the credential's encoded contents. */
export function isCredentialResponse(value: unknown, ceremony: "authentication"): value is AuthenticationResponseJSON;
export function isCredentialResponse(value: unknown, ceremony: "registration"): value is RegistrationResponseJSON;
export function isCredentialResponse(value: unknown, ceremony: Ceremony): value is AuthenticationResponseJSON | RegistrationResponseJSON {
  if (!record(value) || value.type !== "public-key" || !encoded(value.id, 1400) || value.rawId !== value.id || !record(value.clientExtensionResults) || !record(value.response) || !encoded(value.response.clientDataJSON)) return false;
  if (ceremony === "registration") return encoded(value.response.attestationObject)
    && (value.response.transports === undefined || Array.isArray(value.response.transports) && value.response.transports.length <= 10 && value.response.transports.every(t => typeof t === "string" && ["ble", "cable", "hybrid", "internal", "nfc", "smart-card", "usb"].includes(t)));
  return encoded(value.response.authenticatorData) && encoded(value.response.signature)
    && (value.response.userHandle === undefined || value.response.userHandle === null || encoded(value.response.userHandle, 128));
}

function responseClientData(response: AuthenticationResponseJSON | RegistrationResponseJSON): { challenge: string; sameOrigin: boolean } | null {
  try {
    const clientData: unknown = JSON.parse(Buffer.from(response.response.clientDataJSON, "base64url").toString("utf8"));
    return record(clientData) && encoded(clientData.challenge, 128) ? { challenge: clientData.challenge, sameOrigin: clientData.crossOrigin !== true && clientData.topOrigin === undefined } : null;
  } catch { return null; }
}

async function accountLocked(executor: DatabaseExecutor): Promise<Account | null> {
  return (await executor.query<Account>("SELECT *, clock_timestamp() AS now FROM auth.local_account WHERE singleton FOR UPDATE", [])).rows[0] ?? null;
}

function throttled(account: Account): Failure | null {
  return account.locked_until && account.locked_until.getTime() > account.now.getTime()
    ? { status: "throttled", retryAfter: Math.max(1, Math.ceil((account.locked_until.getTime() - account.now.getTime()) / 1000)) } : null;
}

async function rejectAttempt(executor: DatabaseExecutor, account: Account): Promise<Failure> {
  const failures = account.locked_until ? 1 : account.failed_attempts + 1;
  await executor.query("UPDATE auth.local_account SET failed_attempts = $1, locked_until = CASE WHEN $1 >= 5 THEN clock_timestamp() + interval '60 seconds' ELSE NULL END WHERE singleton", [failures]);
  return failures >= 5 ? { status: "throttled", retryAfter: 60 } : { status: "invalid" };
}

async function saveChallenge(executor: DatabaseExecutor, account: Account, challenge: string, browserToken: string, ceremony: Ceremony, grantHash: string | null): Promise<void> {
  await executor.query("INSERT INTO auth.local_webauthn_challenges (challenge_hash, user_id, browser_hash, ceremony, grant_hash, expires_at) VALUES ($1, $2, $3, $4, $5, clock_timestamp() + interval '2 minutes')", [hashToken(challenge), account.user_id, hashToken(browserToken), ceremony, grantHash]);
}

/** Account locking serializes ceremonies, replay/counter updates, credential revocation, and resets. */
export async function authenticationOptions(browserToken: string) {
  return transaction(async executor => {
    const account = await accountLocked(executor);
    if (!account) return { status: "invalid" } as const;
    const limit = await optionLimit(executor, account); if (limit) return limit;
    const keys = (await executor.query<Passkey>("SELECT * FROM auth.local_passkeys WHERE user_id = $1", [account.user_id])).rows;
    if (!keys.length) return { status: "invalid" } as const;
    const options = await generateAuthenticationOptions({ rpID: getLocalAuthConfig().rpId, userVerification: "required", timeout: 60000, allowCredentials: keys.map(key => ({ id: key.credential_id, transports: key.transports })) });
    await saveChallenge(executor, account, options.challenge, browserToken, "authentication", null);
    return { status: "valid", options } as const;
  });
}

async function optionLimit(executor: DatabaseExecutor, account: Account): Promise<Failure | null> {
  const limit = throttled(account); if (limit) return limit;
  await executor.query("DELETE FROM auth.local_webauthn_challenges WHERE expires_at <= clock_timestamp()", []);
  await executor.query("DELETE FROM auth.local_enrollment_grants WHERE expires_at <= clock_timestamp()", []);
  const count = await executor.query<{ count: string }>("SELECT count(*) FROM auth.local_webauthn_challenges WHERE user_id = $1", [account.user_id]);
  return Number(count.rows[0]?.count) >= 32 ? { status: "throttled", retryAfter: 120 } : null;
}

export async function registrationOptions(browserToken: string, grant: string) {
  if (!/^[A-Za-z0-9_-]{43}$/.test(grant)) return { status: "invalid" } as const;
  return transaction(async executor => {
    const account = await accountLocked(executor);
    if (!account) return { status: "invalid" } as const;
    const limit = await optionLimit(executor, account); if (limit) return limit;
    const grantHash = hashToken(grant);
    if (!(await executor.query("SELECT 1 FROM auth.local_enrollment_grants WHERE grant_hash = $1 AND user_id = $2 AND expires_at > clock_timestamp()", [grantHash, account.user_id])).rows.length) return { status: "invalid" } as const;
    const keys = (await executor.query<Passkey>("SELECT * FROM auth.local_passkeys WHERE user_id = $1", [account.user_id])).rows;
    if (keys.length >= 16) return { status: "invalid" } as const;
    const options = await generateRegistrationOptions({
      rpName: "lingvichr", rpID: getLocalAuthConfig().rpId, userName: "Personal", userDisplayName: "lingvichr",
      userID: new Uint8Array(Buffer.from(account.webauthn_user_handle, "base64url")),
      attestationType: "none", supportedAlgorithmIDs: [-7, -257], timeout: 60000,
      authenticatorSelection: { residentKey: "required", userVerification: "required" },
      excludeCredentials: keys.map(key => ({ id: key.credential_id, transports: key.transports })),
    });
    await saveChallenge(executor, account, options.challenge, browserToken, "registration", grantHash);
    return { status: "valid", options } as const;
  });
}

async function consumeChallenge(executor: DatabaseExecutor, browserToken: string, challenge: string | null, ceremony: Ceremony, userId: string): Promise<Challenge | null> {
  if (!challenge) return null;
  const result = await executor.query<Challenge>("DELETE FROM auth.local_webauthn_challenges WHERE challenge_hash = $1 AND browser_hash = $2 AND ceremony = $3 AND user_id = $4 RETURNING grant_hash, expires_at", [hashToken(challenge), hashToken(browserToken), ceremony, userId]);
  return result.rows[0] ?? null;
}

export async function authenticatePasskey(browserToken: string, response: AuthenticationResponseJSON) {
  const clientData = responseClientData(response);
  return transaction(async executor => {
    const account = await accountLocked(executor);
    if (!account) return { status: "invalid" } as const;
    const challenge = await consumeChallenge(executor, browserToken, clientData?.challenge ?? null, "authentication", account.user_id);
    const limit = throttled(account); if (limit) return limit;
    if (!clientData?.sameOrigin || !challenge || challenge.expires_at.getTime() <= Date.now()) return rejectAttempt(executor, account);
    const key = (await executor.query<Passkey>("SELECT * FROM auth.local_passkeys WHERE credential_id = $1 AND user_id = $2", [response.id, account.user_id])).rows[0];
    if (!key || response.response.userHandle && response.response.userHandle !== account.webauthn_user_handle) return rejectAttempt(executor, account);
    const config = getLocalAuthConfig();
    let verification;
    try {
      verification = await verifyAuthenticationResponse({ response, expectedChallenge: clientData.challenge, expectedOrigin: config.authOrigin, expectedRPID: config.rpId, requireUserVerification: true, credential: { id: key.credential_id, publicKey: new Uint8Array(key.public_key), counter: Number(key.counter), transports: key.transports } });
    } catch { return rejectAttempt(executor, account); }
    if (!verification.verified || verification.authenticationInfo.credentialDeviceType !== key.device_type) return rejectAttempt(executor, account);
    await executor.query("UPDATE auth.local_passkeys SET counter = $1, backed_up = $2, last_used_at = clock_timestamp() WHERE credential_id = $3", [verification.authenticationInfo.newCounter, verification.authenticationInfo.credentialBackedUp, key.credential_id]);
    await executor.query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL WHERE singleton", []);
    return { status: "valid", ...await createSession(executor, account.user_id) } as const;
  });
}

export async function registerPasskey(browserToken: string, grant: string, response: RegistrationResponseJSON) {
  const clientData = responseClientData(response);
  return transaction(async executor => {
    const account = await accountLocked(executor);
    if (!account) return { status: "invalid" } as const;
    const challenge = await consumeChallenge(executor, browserToken, clientData?.challenge ?? null, "registration", account.user_id);
    const limit = throttled(account); if (limit) return limit;
    if (!clientData?.sameOrigin || !challenge || challenge.expires_at.getTime() <= Date.now() || challenge.grant_hash !== hashToken(grant)) return rejectAttempt(executor, account);
    const grantRow = await executor.query("SELECT 1 FROM auth.local_enrollment_grants WHERE grant_hash = $1 AND user_id = $2 AND expires_at > clock_timestamp()", [challenge.grant_hash, account.user_id]);
    if (!grantRow.rows.length) return rejectAttempt(executor, account);
    const config = getLocalAuthConfig();
    let verification;
    try {
      verification = await verifyRegistrationResponse({ response, expectedChallenge: clientData.challenge, expectedOrigin: config.authOrigin, expectedRPID: config.rpId, requireUserVerification: true, requireUserPresence: true, supportedAlgorithmIDs: [-7, -257] });
    } catch { return rejectAttempt(executor, account); }
    if (!verification.verified || verification.registrationInfo.credential.id !== response.id) return rejectAttempt(executor, account);
    const { credential, credentialDeviceType, credentialBackedUp } = verification.registrationInfo;
    if (credential.publicKey.length > 4096) return rejectAttempt(executor, account);
    // Use a conflict result, rather than throwing and rolling back the consumed challenge.
    const inserted = await executor.query("INSERT INTO auth.local_passkeys (credential_id, user_id, public_key, counter, transports, device_type, backed_up) VALUES ($1, $2, decode($3, 'hex'), $4, $5, $6, $7) ON CONFLICT DO NOTHING RETURNING credential_id", [credential.id, account.user_id, Buffer.from(credential.publicKey).toString("hex"), credential.counter, credential.transports ?? [], credentialDeviceType, credentialBackedUp]);
    if (!inserted.rows.length) return rejectAttempt(executor, account);
    await executor.query("DELETE FROM auth.local_enrollment_grants WHERE grant_hash = $1", [challenge.grant_hash]);
    await executor.query("UPDATE auth.local_account SET failed_attempts = 0, locked_until = NULL WHERE singleton", []);
    return { status: "valid" } as const;
  });
}
