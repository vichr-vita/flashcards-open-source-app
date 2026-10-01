FROM node:24.21.0-alpine AS builder

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci

COPY tsconfig.json ./
COPY src/ src/
COPY browser/ browser/
COPY scripts/ scripts/
RUN npm run build

FROM node:24.21.0-alpine

WORKDIR /app

RUN addgroup --system --gid 1001 nodejs && \
    adduser --system --uid 1001 auth

COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY --from=builder /app/dist/ dist/

USER auth
EXPOSE 8081
CMD ["node", "dist/index.js"]
