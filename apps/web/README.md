# Web App

Read this file before making any web-specific flow change.

## Goal

The web client should stay aligned with the shared flashcards product contract while feeling immediate, browser-native, and operationally simple to deploy.

The top-level product scope matches the other clients:

- Review
- Cards
- AI
- Settings

## Private browser fork

The private browser installation uses React 19, TypeScript, Vite, TanStack Router, and TanStack
Query. Install dependencies with `pnpm install` at the repository root. The code-based route tree
in `src/App.tsx` preserves public URLs, workspace-scoped URLs, and legacy redirects. Navigation
adapters in `src/routing` preserve encoded query strings and fragment targets.

TanStack Query manages account settings and deduplicates server reads within a verified cookie
session. Query data stays in memory. IndexedDB, the durable outbox, and the existing push/pull
sync flow remain the offline source of truth. Requests with an explicit cancellation signal keep
their existing cancellation ownership, and the transport still owns refresh, CSRF recovery, and
network retries.

Effect Schema validates the shared contract primitives and JSON decoding. Tailwind CSS 4 uses the
`tw:` prefix and omits Preflight so utilities cannot override the shipped CSS classes. The shadcn
button in `src/components/ui/button.tsx` uses Radix Slot and the existing button styles.

Rust generates wire types in `src/generated` with `cargo run --bin lingvichr -- generate-types`.
Card, scheduler, streak-freeze, and review-watermark types derive from those contracts. Runtime
parsers retain the existing stricter enums, metadata validation, and readonly browser collections.

Build the private browser and passkey assets with `pnpm build:web` at the repository root. The Rust
CLI serves those files from `apps/web/dist`; `lingvichr serve --service backend` and
`lingvichr serve --service auth` run the two services with the existing private-host environment
settings. Use a disposable local database for development. The setup and migration commands are
documented in [`apps/server/README.md`](../server/README.md).

`pnpm check` at the repository root runs the private-stack checks with a disposable PostgreSQL
database, real Rust API and passkey authentication, and the browser flows. It does not use production
credentials or the production database. The browser fixture serves `apps/web/dist` at port `19411`
and the Rust backend and auth services at `19400` and `19401`.

## Localization

When adding a new web language, follow [docs/web-localization.md](../../docs/web-localization.md).
That guide covers the real source-of-truth files, browser-local language override behavior, support/error-path audit points, auth locale coordination, and smoke-test expectations.

## Analytics Identity

The web app does not own an anonymous identity of its own. `anonymous_id` is the `analytics_visitor`
cookie the backend mints for the product domain, so this app measures a person from the first page
view and across a logout: [analytics visitor identity](../../docs/analytics-visitor-identity.md).
It does not survive an account deletion, which expires it in this browser so the person continues as
a new anonymous visitor; a deletion started on another device cannot reach this browser's copy.

The auth origin is on that same id, which puts it inside the consent gate. It mints no identity of
its own: its server-side sign-in funnel reports under this cookie and reports nothing for a browser
that holds none, so `app.` and `auth.` measure one visitor.

Where the law requires consent first, a bottom strip asks for it, and until the person answers this
app writes nothing to the device and sends nothing carrying an identifier. The banner also asks on
the public catalog, invite and share routes, and the same switch withdraws on both sides of the
session gate: the settings screen at `/settings/analytics` for a signed-in person, and a corner link
on those public routes for a visitor with no account. Both render the one switch in
`src/analytics/AnalyticsConsentToggleCard.tsx`, and both carry the answer to the account whenever one
is signed in on this browser. Read [analytics visitor identity](../../docs/analytics-visitor-identity.md) before
touching the banner, either withdrawal entry, or anything in `src/analytics/` that runs before a
decision exists.

The banner decides only that identifier. Whether this person is measured at all is a second, separate
switch beside it on the same two surfaces: on, because the basis is legitimate interest, and while it
is off the client sends nothing, queues nothing and holds nothing. Refusing the cookie is not
refusing analytics, and neither answer is ever read into the other
(`src/analytics/productAnalyticsCollection.ts`).

Which transport an event leaves on follows from whether a credential exists. A signed-in browser
batches through the authenticated ingest; a signed-out one reports one event per request through the
credential-free collector, [anonymous client analytics](../../docs/anonymous-client-analytics.md).
Events collected under an account never leave credential-free, and neither does anything while this
browser says an account owns it: a signed-in person reporting `app_opened` only through the collector
would be read as an actor with no `app_opened` at all, and auto-excluded from every person-level
report until a human restores them.

## Browser tests and upstream reference

The web app uses the browser-native test stack already present in this package:

- targeted Vitest checks can cover real module boundaries in the web package, but we do not aim for exhaustive unit coverage
- release-gate browser coverage runs with Playwright in `apps/web/e2e/live-smoke.spec.ts`, grouped into shared-session smoke flows

Prefer the Playwright live smoke when a change affects a real user flow. It is the highest-confidence web check because it exercises the shipped browser app closest to production.

The live smoke scenario intentionally mirrors the mobile clients:

- iOS equivalent: `apps/ios/Flashcards/FlashcardsUITests/LiveSmokeUITests.swift`
- Android equivalent: `apps/android/app/src/androidTest/java/com/flashcardsopensourceapp/app/livesmoke/LiveSmokeTest.kt`

The upstream Cognito smoke remains as reference for the supported upstream clients. It is separate
from the private Rust-stack checks above. `pnpm test:e2e:local` in `apps/web` runs that upstream
Playwright smoke against its local browser stack:

1. local auth on `http://localhost:8081`
2. local backend on `http://localhost:8080`
3. local production-style web preview on `http://localhost:3000` that Playwright builds and serves automatically

This local smoke does not reuse production auth. It is intentionally isolated so localhost never depends on the deployed auth allowlist or production web origin.

Local smoke prerequisites:

- root `.env` must keep `AUTH_MODE=cognito`
- local auth/backend must have real Cognito config (`COGNITO_USER_POOL_ID`, `COGNITO_CLIENT_ID`, `COGNITO_REGION`, `SESSION_ENCRYPTION_KEY`)
- review account sign-in should be enabled locally with `DEMO_EMAIL_DOSTIP` and `DEMO_PASSWORD_DOSTIP`
- start the local data/auth stack first with `make db-up`, `make auth-dev`, and `make backend-dev`

The local smoke preflight fails fast if local auth or backend is unavailable, or if the Playwright target is misconfigured to mix localhost with deployed origins.

`pnpm test:e2e:review-ui` runs a focused Chromium integration flow against the real review
components and keyboard handlers with an in-memory card queue. It checks dark and light themes,
desktop and mobile layouts, the reveal flip, reduced motion, rating advancement, and long markdown
answers. It also checks source URL wrapping, filter overlays, and long-answer scrolling in
Chromium and WebKit at narrow mobile widths. It needs no auth or backend and runs in the web PR
checks. Its isolated Vite server uses
port `4318`; it does not use the usual local app preview or any production account.

The same suite exercises the real IndexedDB card and review queries in Chromium and WebKit.
It injects a lost cursor after records have been read, then checks the single retry, accurate
counts and pagination, retained offline operations, and transaction abort handling.

The navigation/server-query smoke runs in Chromium and WebKit at 320px. It checks repeated query
parameters, encoded fragments, browser Back/Forward, account-scoped server reads, duplicate-read
deduplication, mutation CSRF headers, retry, and the existing error dialog with long details. Its
server responses are isolated Playwright fixtures, with no production requests.

`pnpm test:e2e` and `pnpm test:e2e:prod` remain upstream deployed smoke entrypoints. Run them only
when explicitly testing that upstream deployment.

## Respect Existing Code

Before making any change, read the existing components and modules in the area you are touching.

Follow the patterns already present:

- match the component structure, state management approach, and API call conventions already used in neighboring screens
- use the same naming conventions for components, hooks, types, and test selectors already established in the codebase
- if a shared hook, utility, or helper already exists for what you need, use it instead of adding a new one
- do not introduce a new abstraction or pattern unless the existing one is clearly broken for the task at hand

If you are unsure how something is done, read two or three existing screens or hooks first. The answer is almost always already there.

Error messages reach Sentry verbatim, so never interpolate user-authored content into one: no card text, deck names, email addresses, or chat messages. Identifiers, endpoints, field names, sizes, and status codes are fine.

## CI/CD

The private rewrite uses `.github/workflows/pr-checks.yml` and `pnpm check`. The workflow tests the
Rust and browser stack without deploying it. The AWS release notes below describe the preserved
upstream deployment reference.

Web build and deploy details are documented in [`docs/backend-web-deployment.md`](../../docs/backend-web-deployment.md).

The expected main-branch release flow is:

1. Native web build in GitHub Actions
2. Web deploy on `main`
3. Native Playwright live smoke after deploy as an operational signal

When a change lands on `main`, follow the GitHub `AWS/Web Release` workflow through completion instead of assuming the web release finished automatically. A failed web live smoke is visible after deploy, leaves the deployed release in place, and should be fixed forward in the next iteration. Do not try to guard every internal web detail with tests; add only the smallest targeted check that validates an important boundary or user flow.
