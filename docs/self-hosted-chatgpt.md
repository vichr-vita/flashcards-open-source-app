# ChatGPT subscriptions on a private server

This fork can use a ChatGPT subscription for chat and card tools through Codex's device-code login. Sign in from **Settings > AI settings**. The browser displays a code to enter on OpenAI's site; no SSH callback tunnel is needed.

This integration supports the single-account, private web installation described in [Self-hosted passkey login](self-hosted-local-auth.md). Native clients and the public Cognito deployment do not expose this connection. Deployment still requires separate authorization.

## Configure the backend

Keep the existing local authentication settings and add these backend variables:

| Variable | Value |
| --- | --- |
| `AUTH_MODE` | `local` |
| `CHATGPT_CONNECTION_DIR` | An absolute persistent directory outside the checkout, for example `/var/lib/nibomo/chatgpt-credentials` |
| `CHAT_LIVE_URL` | The browser-reachable backend URL ending in `/v1/chat/live` |

The credential directory must belong to the backend process UID and have mode `0700`. The backend creates `connection.json` with mode `0600` and refuses files owned by another UID or readable by other users. Use a persistent bind mount when running in Docker. Treat the directory and its backups as account credentials. Do not commit, serve, or log its contents.

Run one backend process for this installation. Token refresh, sign-in polling, and disconnect are serialized in that process. Pending sign-in expires after 15 minutes and is lost on restart; completed connections persist. A stopped process also interrupts its chat runs, which the existing stale-run recovery handles.

No database migration is required. Local mode executes the chat worker in the backend process and serves the existing signed live-stream protocol at `/v1/chat/live`. It uses the persistent backend CSRF secret to derive the stream-signing key and does not need AWS worker or live-secret settings. Preserve the existing allowed origins, cookies, and private access boundary.

## Connect your subscription

1. Enable device-code login in your [ChatGPT security settings](https://learn.chatgpt.com/docs/auth#preferred-device-code-authentication-beta).
2. Sign in to the private app with your passkey. Open **Settings > AI settings** and select **Connect ChatGPT**.
3. Copy the displayed code. Select **Open OpenAI sign-in**, authenticate on OpenAI's site, and enter the code there.
4. Keep the app open while it polls. When the account appears, choose an available model and **Reasoning effort**, then send a chat message. Higher effort can take longer.

OAuth tokens remain on the server. The connection belongs to the local application account, so its signed-in devices use the same subscription. Model and effort selections save immediately and persist across backend restarts. Each chat turn captures the connection, model, and effort selected when it starts. Later changes affect new turns.

The effort dropdown shows the levels advertised for the selected model by your account's model catalog. Switching models preserves the effort if supported, otherwise it selects the new model's advertised default or first supported level. Models without effort levels disable the dropdown and omit effort from requests. Existing connections automatically load this metadata without another sign-in, refreshing expired tokens first.

**Disconnect** removes the stored tokens and cancels pending sign-in. It keeps ChatGPT selected, so the next turn asks you to reconnect or explicitly select API. It does not revoke OpenAI's grant, and an upstream request already sent may finish. Use OpenAI's account controls if you also want to revoke the authorization.

## Supported AI operations

Chat and the existing workspace/card tools use the subscription. ChatGPT account limits still apply. A failed subscription request never switches automatically to a billed API request. Choose **OpenAI API** yourself to use the existing personal key or configured server key.

Dictation requires the existing API setup. Image generation also requires an API key and the existing media storage setup; subscription chat excludes the image-generation tool. The API-funded follow-up suggestion request is skipped for subscription turns.

Usage records identify the actual subscription model and use the existing personal-credential flag to exclude those tokens from platform-funded AI limits. The chat run's existing API model and cost-policy metadata remain unchanged; the worker receives the subscription model separately. Subscription model selection therefore does not redefine the app's API pricing metadata.

Reasoning items are available within a turn but are excluded from subscription history replay. Workspace tool results continue through the existing chat loop.

## Protocol and maintenance

The adapter follows the older public Codex protocol selected for this private installation. It does not implement the newer Sign in with ChatGPT integration. The device-code endpoints, public client identifier, token exchange, model catalog, and Responses transport can change upstream. The model catalog currently sends `client_version=0.159.3`.

The implementation was checked on October 1, 2026 against OpenAI's Codex source at commit `ecc78e4cf5607ecf5f080d682eae0ecb650868ae`:

- [Device-code login](https://github.com/openai/codex/blob/ecc78e4cf5607ecf5f080d682eae0ecb650868ae/codex-rs/login/src/device_code_auth.rs).
- [Authorization-code exchange](https://github.com/openai/codex/blob/ecc78e4cf5607ecf5f080d682eae0ecb650868ae/codex-rs/login/src/server.rs).
- [Token refresh](https://github.com/openai/codex/blob/ecc78e4cf5607ecf5f080d682eae0ecb650868ae/codex-rs/login/src/auth/manager.rs).

Live sign-in with a real subscription has not been verified. The automated provider fixture validates the implemented protocol and app integration without contacting OpenAI.

## Verification

Run the disposable PostgreSQL/auth/backend flow documented in [the local-auth integration guide](self-hosted-local-auth.md#run-the-isolated-integration-check). It now also starts a simulated OpenAI provider on loopback port `19402`. It exercises device-code sign-in, protected settings routes, private credential storage, model and effort selection, legacy connection upgrades, refresh, a real workspace tool call, streamed replies, provider limits, and disconnect. It checks that every provider request in a turn uses its captured effort, even after a settings change or token refresh. No real account or paid API is used.

For browser review, set `LOCAL_CHATGPT_BROWSER_REVIEW=true` for that integration command. Start an isolated web process on port `19411` with API base `http://localhost:19400/v1`, auth base `http://localhost:19401`, and app base `http://localhost:19411`. Open the fixture login URL printed by the integration process. Press Enter in its terminal when finished so it removes the throwaway account and credential directory. Use these settings only for the disposable loopback fixture.

After an authorized deployment, verify the real account manually:

1. Complete device-code sign-in and send a chat message. Ask the assistant to create a card and confirm the front contains the question and the back contains the answer.
2. Change **Reasoning effort**, reload AI settings, and confirm the choice persists. Change models and confirm the available efforts follow the model. Send a new turn and change effort while it runs; the next turn should use the new choice. Check cancellation of pending sign-in and expiry of an unused code. Verify a provider error leaves an actionable message.
3. Restart the private backend through its established runbook and verify the connection persists. Verify refresh after the access token expires.
4. Disconnect and confirm new turns require reconnection. Select API explicitly and verify its separate key setup. Check dictation separately with the API configuration.
5. Repeat sign-in and chat on the real phone. A desktop browser at phone width does not establish iOS Home Screen behavior.
