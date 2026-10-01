/** Fail closed before serving the local provider; HTTP is only available on loopback in development. */
export function getLocalAuthConfig() {
  const allowHttp = process.env.NODE_ENV === "development" && process.env.LOCAL_AUTH_ALLOW_HTTP === "true";
  function parseOrigin(value: string): string {
    const url = new URL(value);
    const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1";
    if (url.username || url.password || (url.protocol !== "https:" && !(allowHttp && loopback && url.protocol === "http:"))) {
      throw new Error("Local auth origins require HTTPS, except explicitly enabled development loopback HTTP");
    }
    if (url.pathname !== "/" || url.search || url.hash) throw new Error("Local auth origins must not include paths, queries, or fragments");
    return url.origin;
  }
  const authOrigin = parseOrigin(process.env.PUBLIC_AUTH_BASE_URL ?? "");
  const rpId = process.env.WEBAUTHN_RP_ID ?? "";
  if (!rpId || rpId !== new URL(authOrigin).hostname || (rpId !== "localhost" && (!rpId.includes(".") || /^[\d.]+$/.test(rpId)))) {
    throw new Error("WEBAUTHN_RP_ID must equal the auth hostname, without a port; use a DNS hostname or development localhost");
  }
  const redirectOrigins = (process.env.ALLOWED_REDIRECT_URIS ?? "").split(",").filter(Boolean).map(value => parseOrigin(value.trim()));
  if (redirectOrigins.length === 0) throw new Error("ALLOWED_REDIRECT_URIS is required for local auth");
  if (!process.env.DATABASE_URL || process.env.DB_SECRET_ARN) throw new Error("Local auth requires DATABASE_URL and does not use DB_SECRET_ARN");
  return { rpId, authOrigin, redirectOrigins, allowHttp };
}
