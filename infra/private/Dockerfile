FROM node:24.21.0-bookworm-slim AS browser
WORKDIR /source
RUN npm install --global pnpm@12.5.1
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/package.json
COPY apps/docs/package.json apps/docs/package.json
RUN pnpm install --frozen-lockfile
COPY apps/web apps/web
COPY apps/auth/browser apps/auth/browser
# The browser shares only this pure offline scheduler with the retained upstream source.
COPY apps/backend/src/scheduling/index.ts apps/backend/src/scheduling/workspaceConfig.ts apps/backend/src/scheduling/
COPY scripts/build-passkey.mjs scripts/build-passkey.mjs
ARG PUBLIC_APP_BASE_URL=http://localhost:19410
ARG PUBLIC_API_BASE_URL=http://localhost:19410/v1
ARG PUBLIC_AUTH_BASE_URL=http://localhost:19410
ENV VITE_APP_BASE_URL=$PUBLIC_APP_BASE_URL VITE_API_BASE_URL=$PUBLIC_API_BASE_URL VITE_AUTH_BASE_URL=$PUBLIC_AUTH_BASE_URL
RUN pnpm build:web

FROM rust:slim-bookworm AS compiler
RUN apt-get update && apt-get install --yes --no-install-recommends pkg-config libssl-dev cmake clang && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml ./
COPY apps/server apps/server
COPY vendor/webauthn-rs-core vendor/webauthn-rs-core
RUN cargo build --locked --release --bin lingvichr

FROM debian:bookworm-slim AS server
ARG APP_UID=1001
ARG APP_GID=1001
RUN apt-get update && apt-get install --yes --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/* && groupadd --gid "$APP_GID" lingvichr && useradd --uid "$APP_UID" --gid "$APP_GID" --create-home lingvichr
WORKDIR /app
COPY --from=compiler /source/target/release/lingvichr /usr/local/bin/lingvichr
COPY --from=browser /source/apps/web/dist /app/web
COPY db /app/db
USER lingvichr
EXPOSE 19410
ENTRYPOINT ["lingvichr"]
CMD ["serve", "--bind", "0.0.0.0:19410", "--service", "all", "--web-dir", "/app/web"]
