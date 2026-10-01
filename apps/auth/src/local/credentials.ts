import { createCipheriv, createDecipheriv, createHash, randomBytes } from "node:crypto";
import { Algorithm, hash } from "@node-rs/argon2";
import { TOTP, Secret } from "otpauth";
import { getLocalAuthConfig } from "./config.js";

export function hashPassword(password: string): Promise<string> {
  if (password.length < 12 || Buffer.byteLength(password) > 1024) {
    throw new Error("Password must have at least 12 characters and at most 1024 bytes");
  }
  return hash(password, { algorithm: Algorithm.Argon2id, memoryCost: 65536, timeCost: 3, parallelism: 1 });
}

export function createAuthenticator(secret = new Secret({ size: 20 })) {
  return new TOTP({ issuer: "Nibomo", label: "Personal", algorithm: "SHA1", digits: 6, period: 30, secret });
}

export function hashToken(token: string): string {
  return createHash("sha256").update(token).digest("hex");
}

export function newToken(): string {
  return randomBytes(32).toString("base64url");
}

export function encryptSecret(secret: string): string {
  const iv = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", getLocalAuthConfig().encryptionKey, iv);
  cipher.setAAD(Buffer.from("nibomo:local-totp:v1"));
  const ciphertext = Buffer.concat([cipher.update(secret, "utf8"), cipher.final()]);
  return Buffer.concat([iv, cipher.getAuthTag(), ciphertext]).toString("base64");
}

export function decryptSecret(encrypted: string): Secret {
  const data = Buffer.from(encrypted, "base64");
  const decipher = createDecipheriv("aes-256-gcm", getLocalAuthConfig().encryptionKey, data.subarray(0, 12));
  decipher.setAAD(Buffer.from("nibomo:local-totp:v1"));
  decipher.setAuthTag(data.subarray(12, 28));
  return Secret.fromBase32(Buffer.concat([decipher.update(data.subarray(28)), decipher.final()]).toString("utf8"));
}
