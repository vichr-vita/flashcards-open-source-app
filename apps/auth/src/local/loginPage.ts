/** Option A: both factors together, with no first-factor session or public setup flow. */
export function renderLocalLoginPage(csrfToken: string, redirectUri: string, nonce: string): string {
  const redirectJson = JSON.stringify(redirectUri).replace(/</g, "\\u003c");
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="color-scheme" content="dark"><title>Sign in · Nibomo</title>
<style nonce="${nonce}">
*{box-sizing:border-box}body{margin:0;background:#000;color:#fff;font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}main{min-height:100svh;display:grid;align-content:center;padding:32px 24px}.login{width:100%;max-width:300px;margin:0 auto}.brand{text-align:center;font-size:17px;font-weight:600;letter-spacing:-.045em}h1{font-size:27px;line-height:1.2;font-weight:600;letter-spacing:-.035em;margin:32px 0 24px}.field{margin-bottom:20px}label{display:block;font-size:14px;margin-bottom:8px}input{display:block;width:100%;min-height:46px;border:1px solid #666;border-radius:0;background:#111;color:#fff;padding:10px 12px;font:inherit}input::placeholder{color:#bcbcbc;opacity:1}#code{font-family:ui-monospace,"SFMono-Regular",Consolas,monospace;letter-spacing:.07em}.hint{font-size:13px;color:#bcbcbc;margin:8px 0 0}button{width:100%;min-height:46px;border:1px solid #fff;border-radius:0;padding:10px 18px;background:#fff;color:#000;font:inherit;font-weight:600;cursor:pointer}button:disabled{cursor:wait;opacity:.65}input:focus-visible,button:focus-visible{outline:2px solid #fff;outline-offset:4px}#error{color:#ffada5;font-size:14px;margin:0 0 16px}#error:empty{display:none}#status{font-size:13px;margin:12px 0 0}noscript{display:block;margin-top:16px;font-size:14px}
</style></head><body><main><div class="login"><div class="brand">nibomo</div><h1>Sign in</h1>
<form id="login"><div class="field"><label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" placeholder="Your password" required maxlength="1024"></div>
<div class="field"><label for="code">Authenticator code</label><input id="code" name="code" type="text" inputmode="numeric" autocomplete="one-time-code" placeholder="6-digit code" pattern="[0-9]{6}" maxlength="6" required><p class="hint">Use the code from your authenticator app.</p></div>
<p id="error" role="alert" aria-live="polite"></p><button id="submit" type="submit" disabled>Sign in</button><p id="status" role="status">Checking session...</p></form><noscript>Enable JavaScript to sign in.</noscript>
</div></main><script nonce="${nonce}">
(() => {
  const csrfToken = "${csrfToken}";
  const redirectUri = ${redirectJson};
  const form = document.getElementById("login");
  const password = document.getElementById("password");
  const code = document.getElementById("code");
  const button = document.getElementById("submit");
  const error = document.getElementById("error");
  const status = document.getElementById("status");
  code.addEventListener("input", () => { code.value = code.value.replace(/\\D/g, "").slice(0, 6); });
  fetch("/api/refresh-session", {method:"POST",credentials:"same-origin"}).then(response => {
    if (response.ok) location.replace(redirectUri);
  }).catch(() => {}).finally(() => { button.disabled = false; status.textContent = ""; password.focus(); });
  form.addEventListener("submit", async event => {
    event.preventDefault();
    if (button.disabled) return;
    button.disabled = true; button.textContent = "Signing in..."; error.textContent = "";
    try {
      const response = await fetch("/api/login", {method:"POST",credentials:"same-origin",headers:{"Content-Type":"application/json","X-CSRF-Token":csrfToken},body:JSON.stringify({password:password.value,code:code.value})});
      if (response.ok) { password.value = ""; code.value = ""; location.replace(redirectUri); return; }
      const data = await response.json();
      error.textContent = data.error || "Sign-in failed. Try again.";
      code.value = "";
      if (data.code === "LOGIN_CSRF_INVALID") status.textContent = "Reload this page to try again.";
    } catch { error.textContent = "Cannot reach the server. Try again."; }
    finally { button.disabled = false; button.textContent = "Sign in"; }
  });
})();</script></body></html>`;
}
