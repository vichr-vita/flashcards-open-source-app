COMPOSE_FILE=infra/private/compose.yml

up:
	docker compose -f $(COMPOSE_FILE) up -d --build

down:
	docker compose -f $(COMPOSE_FILE) down

dev:
	docker compose -f $(COMPOSE_FILE) up --build

build:
	pnpm install --frozen-lockfile
	cargo build --locked --release
	pnpm build:web
	pnpm build:docs

lint:
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets -- -D warnings
	pnpm --dir apps/web exec tsc --noEmit
	pnpm --dir apps/docs check

migrate:
	cargo run --locked -- migrate

migrate-aws:
	bash scripts/deploy/migrate-aws.sh

check-api-health:
	bash scripts/checks/check-api-health.sh

check-public-endpoints:
	bash scripts/checks/check-public-endpoints.sh

auth-dev:
	cargo run --locked -- serve --service auth --bind 127.0.0.1:19401

backend-dev:
	cargo run --locked -- serve --service backend --bind 127.0.0.1:19400

web-dev:
	pnpm --dir apps/web dev

admin-dev:
	cd apps/admin && node --env-file=../../.env ./node_modules/vite/bin/vite.js
