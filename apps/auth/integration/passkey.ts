/** Software authenticator fixture with real ES256 and legacy RS256 signatures. */
import { createHash, generateKeyPairSync, randomBytes, sign, type KeyObject } from "node:crypto";
import { isoCBOR } from "@simplewebauthn/server/helpers";
import type { AuthenticationResponseJSON, RegistrationResponseJSON, PublicKeyCredentialCreationOptionsJSON, PublicKeyCredentialRequestOptionsJSON } from "@simplewebauthn/server";

function hash(value: string | Buffer): Buffer { return createHash("sha256").update(value).digest(); }
export class TestPasskey {
  readonly id = randomBytes(32).toString("base64url");
  private readonly pair: { publicKey: KeyObject; privateKey: KeyObject };
  private counter = 0;
  private userHandle = "";
  constructor(private readonly synced = false, rsaBits?: 3072 | 4096) {
    this.pair = rsaBits === undefined
      ? generateKeyPairSync("ec", { namedCurve: "prime256v1" })
      : generateKeyPairSync("rsa", { modulusLength: rsaBits });
  }
  registration(options: PublicKeyCredentialCreationOptionsJSON, override: { origin?: string; rpId?: string; flags?: number; crossOrigin?: boolean; publicKey?: Uint8Array } = {}): RegistrationResponseJSON {
    this.userHandle = options.user.id;
    const key = this.pair.publicKey.export({ format: "jwk" });
    const publicKey = override.publicKey ?? isoCBOR.encode(key.kty === "RSA"
      ? new Map<number, number | Uint8Array>([[1, 3], [3, -257], [-1, new Uint8Array(Buffer.from(key.n!, "base64url"))], [-2, new Uint8Array(Buffer.from(key.e!, "base64url"))]])
      : new Map<number, number | Uint8Array>([[1, 2], [3, -7], [-1, 1], [-2, new Uint8Array(Buffer.from(key.x!, "base64url"))], [-3, new Uint8Array(Buffer.from(key.y!, "base64url"))]]));
    const id = Buffer.from(this.id, "base64url");
    const length = Buffer.alloc(2); length.writeUInt16BE(id.length);
    const authData = Buffer.concat([hash(override.rpId ?? options.rp.id!), Buffer.from([override.flags ?? (this.synced ? 0x5d : 0x45)]), Buffer.alloc(4), Buffer.alloc(16), length, id, publicKey]);
    const attestationObject = isoCBOR.encode(new Map<string, string | Uint8Array | Map<string, string>>([["fmt", "none"], ["authData", new Uint8Array(authData)], ["attStmt", new Map()]]));
    return { id: this.id, rawId: this.id, type: "public-key", clientExtensionResults: {}, response: { clientDataJSON: Buffer.from(JSON.stringify({ type: "webauthn.create", challenge: options.challenge, origin: override.origin ?? "http://localhost:19401", crossOrigin: override.crossOrigin ?? false })).toString("base64url"), attestationObject: Buffer.from(attestationObject).toString("base64url"), transports: ["internal"] } };
  }
  assertion(options: PublicKeyCredentialRequestOptionsJSON, override: { origin?: string; rpId?: string; flags?: number; challenge?: string; crossOrigin?: boolean; wrongSignature?: boolean; userHandle?: string; counter?: number } = {}): AuthenticationResponseJSON {
    const counter = Buffer.alloc(4); counter.writeUInt32BE(override.counter ?? (this.synced ? 0 : ++this.counter));
    const authenticatorData = Buffer.concat([hash(override.rpId ?? options.rpId!), Buffer.from([override.flags ?? (this.synced ? 0x1d : 0x05)]), counter]);
    const clientData = Buffer.from(JSON.stringify({ type: "webauthn.get", challenge: override.challenge ?? options.challenge, origin: override.origin ?? "http://localhost:19401", crossOrigin: override.crossOrigin ?? false }));
    const signature = sign("sha256", Buffer.concat([authenticatorData, hash(clientData)]), this.pair.privateKey);
    if (override.wrongSignature) signature[signature.length - 1] ^= 1;
    return { id: this.id, rawId: this.id, type: "public-key", clientExtensionResults: {}, response: { clientDataJSON: clientData.toString("base64url"), authenticatorData: authenticatorData.toString("base64url"), signature: signature.toString("base64url"), userHandle: override.userHandle ?? this.userHandle } };
  }
}
