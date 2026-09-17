-include .env
export

RUN_DIR := _running
PID_DIR := $(RUN_DIR)/pids
LOG_DIR := $(RUN_DIR)/logs
PID_FILE := $(PID_DIR)/avalon-server.pid
LOG_FILE := $(LOG_DIR)/avalon-server.log

.PHONY: help \
	build run start stop restart status test test-live fmt fmt-check lint sdk-examples sdk-doc check clean \
	migrate migrate-down db-reset \
	stack-up stack-up-no-redis stack-down stack-logs \
	web-install hub-dev mobile-dev storybook web-build web-lint web-test \
	csharp-build csharp-test \
	inspect-ledger inspect-ledger-full create-identity login outbox-status \
	register-integrator issue-achievement \
	check-all clean-all

help:
	@echo "avalon-protocol — local dev commands"
	@echo ""
	@echo "Rust workspace (crates/) — the protocol, chain, indexer, server, sdk, cli"
	@echo "  make build         cargo build --workspace"
	@echo "  make run           run avalon-server in the foreground"
	@echo "  make start         run avalon-server in the background (pid/log under $(RUN_DIR)/)"
	@echo "  make stop          stop what 'make start' started"
	@echo "  make restart       stop, then start"
	@echo "  make status        report whether the background process is running"
	@echo "  make test          cargo test --workspace"
	@echo "  make test-live     cargo test --workspace -- --ignored"
	@echo "  make fmt           cargo fmt --all"
	@echo "  make fmt-check     cargo fmt --all -- --check"
	@echo "  make lint          cargo clippy --workspace --all-targets -- -D warnings"
	@echo "  make migrate       apply pending db/migrations/ (up)"
	@echo "  make migrate-down  revert the most recently applied migration"
	@echo "  make db-reset      wipe the database (drop+recreate public schema) and reapply all migrations"
	@echo "  make stack-up          Docker Compose bring-up: postgres + redis + avalon-server, migrated and started, no Rust/Node toolchain needed (issue #289/#545)"
	@echo "  make stack-up-no-redis same as stack-up, without the Redis-backed rate limit/concurrency ceiling (issue #545)"
	@echo "  make stack-down        stop what 'make stack-up'/'make stack-up-no-redis' started"
	@echo "  make stack-logs        follow avalon-server's logs inside the compose stack"
	@echo "  make check         fmt-check + lint + test — what CI runs"
	@echo "  make clean         remove Rust build artifacts and PID/log files"
	@echo ""
	@echo "JS/TS workspace (apps/hub, apps/mobile-hub, packages/ui)"
	@echo "  make web-install   npm install at the workspace root"
	@echo "  make hub-dev       vite dev server for apps/hub (web Hub client)"
	@echo "  make mobile-dev    tauri dev for apps/mobile-hub (desktop/mobile companion app)"
	@echo "  make storybook     Storybook for packages/ui (shared component library)"
	@echo "  make web-build     build hub, mobile-hub, and ui across the JS workspace"
	@echo "  make web-lint      eslint across the JS workspace"
	@echo "  make web-test      vitest across the JS workspace"
	@echo ""
	@echo "C# SDK (bindings/csharp) — flagship external SDK for game developers"
	@echo "  make csharp-build  dotnet build bindings/csharp/AvalonSdk.sln"
	@echo "  make csharp-test   dotnet test bindings/csharp/AvalonSdk.sln"
	@echo ""
	@echo "  make inspect-ledger        pretty-print the hash-chained ledger (avalon-cli)"
	@echo "  make inspect-ledger-full   same, plus each entry's actual payload"
	@echo "  make create-identity  register a new self-custodied (passkey) identity via avalon-cli"
	@echo "  make login IDENTITY_ID=<uuid>  log in an identity create-identity saved locally, print a session token"
	@echo "  make outbox-status    pending/oldest-pending count for the settlement outbox (issue #71)"
	@echo "  make register-integrator SLUG=<slug> NAME=<name> OWNER=<owner>  register a test integrator, save its key locally"
	@echo "  make issue-achievement INTEGRATOR=<slug> ACHIEVEMENT=<key> TOKEN=<session-token>  issue an already-defined achievement to the identity behind TOKEN"
	@echo ""
	@echo "  make check-all     check (Rust) + web-lint + web-test + csharp-build + csharp-test"
	@echo "  make clean-all     clean (Rust) + remove node_modules/dist + dotnet bin/obj"

# --- Rust workspace ----------------------------------------------------------

build:
	cargo build --workspace

run:
	cargo run -p avalon-server

# Generic PID-file-based background run — reusable as-is for any long-running
# process. Only `run:` above needs to change per-stack; start/stop/status
# don't.
start:
	@mkdir -p $(PID_DIR) $(LOG_DIR)
	@if [ -f $(PID_FILE) ] && kill -0 "$$(cat $(PID_FILE))" 2>/dev/null; then \
		echo "already running (pid $$(cat $(PID_FILE)))"; \
	else \
		( $(MAKE) run > $(LOG_FILE) 2>&1 & echo $$! > $(PID_FILE) ); \
		sleep 1; \
		echo "started (pid $$(cat $(PID_FILE))), logs: $(LOG_FILE)"; \
	fi

stop:
	@if [ -f $(PID_FILE) ] && kill -0 "$$(cat $(PID_FILE))" 2>/dev/null; then \
		kill "$$(cat $(PID_FILE))"; \
		rm -f $(PID_FILE); \
		echo "stopped"; \
	else \
		echo "not running"; \
		rm -f $(PID_FILE); \
	fi

restart: stop start

status:
	@if [ -f $(PID_FILE) ] && kill -0 "$$(cat $(PID_FILE))" 2>/dev/null; then \
		echo "running (pid $$(cat $(PID_FILE)))"; \
	else \
		echo "not running"; \
	fi

test:
	cargo test --workspace

# Delete this target (and its help line above) if this project ends up with no
# tests gated on real infra (a database, an external service) that are skipped
# by default in `test`.
test-live:
	cargo test --workspace -- --ignored

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

# `avalon-sdk`'s runnable examples (issue #49) — `--all-targets` above
# already lints them, but a dedicated build step catches an example that
# compiles-but-clippy-skips (unlikely) and is what the ticket itself names
# as the acceptance check.
sdk-examples:
	cargo build -p avalon-sdk --examples

# `#![deny(missing_docs)]` (issue #49) is enforced by `cargo build`/`check`
# already; this additionally catches broken intra-doc links, which are a
# doc-only warning `cargo build` never sees.
sdk-doc:
	RUSTDOCFLAGS="-D warnings" cargo doc -p avalon-sdk --no-deps

check: fmt-check lint test sdk-examples sdk-doc

clean:
	cargo clean
	rm -rf $(RUN_DIR)

# --- Database (db/migrations/<version>_<description>/{up,down}.sql) --------

migrate:
	cargo run -p avalon-server --bin migrate -- up

migrate-down:
	cargo run -p avalon-server --bin migrate -- down

db-reset:
	cargo run -p avalon-server --bin migrate -- reset

# --- One-command hoster bring-up (issue #289) -------------------------------
# No Rust/Node toolchain needed on the host — Docker Compose builds and runs
# avalon-server + Postgres from a fresh checkout. Distinct from
# start/stop/status above, which assume a local cargo/npm dev setup; this is
# the hoster-facing path #288 tracks getting to a non-technical-friendly bar.
#
# First run generates .env from .env.compose.example if none exists, filling
# in the two values that genuinely can't have a shared default — a signing
# key (required, no fallback — see crates/chain/src/sth.rs) and a
# network_id unique to this deployment (avoids two independently-brought-up
# stacks colliding if their databases were ever pointed at each other) —
# rather than leaving a placeholder that silently breaks later, per this
# ticket's own invariant. Re-running against an already-running stack is a
# no-op/clean restart: `.env` generation is skipped once it exists, and
# `docker compose up -d` is itself idempotent.
#
# Issue #545: `stack-up` includes Redis-backed rate limiting/concurrency
# ceiling by default, via the `docker-compose.redis.yml` *override* file
# (not folded into docker-compose.yml directly — see that file's own
# comment for why AVALON_REDIS_URL can't just be "present but harmless").
# `stack-up-no-redis` is the opt-out — today's `stack-up` only runs one
# avalon-server container, so Redis genuinely does nothing useful yet on
# its own; it's on by default so a hoster who later scales to more than
# one replica already has it running, rather than silently reverting to
# per-process limits the moment they do.
STACK_COMPOSE_FILES := -f docker-compose.yml -f docker-compose.redis.yml

stack-up:
	@if [ ! -f .env ]; then \
		echo "no .env found — generating one from .env.compose.example"; \
		cp .env.compose.example .env; \
		sed -i.bak "s/^AVALON_NETWORK_ID=.*/AVALON_NETWORK_ID=avalon-stack-$$(python3 -c 'import secrets; print(secrets.token_hex(4))')/" .env && rm -f .env.bak; \
		echo "" >> .env; \
		echo "# Generated by 'make stack-up' — unique to this deployment, do not share or commit." >> .env; \
		echo "AVALON_SETTLEMENT_SIGNING_KEY=$$(python3 -c 'import secrets; print(secrets.token_hex(32))')" >> .env; \
		chmod 600 .env; \
		DISCOVERED=$$(docker compose $(STACK_COMPOSE_FILES) --profile stack run --rm discover-mirror-peers); \
		if [ -n "$$DISCOVERED" ]; then \
			echo "AVALON_MIRROR_PEERS=$$DISCOVERED" >> .env; \
			echo "discover-mirror-peers: configured AVALON_MIRROR_PEERS=$$DISCOVERED"; \
		fi; \
	fi
	docker compose $(STACK_COMPOSE_FILES) --profile stack up -d --build postgres redis
	docker compose $(STACK_COMPOSE_FILES) --profile stack run --rm migrate
	docker compose $(STACK_COMPOSE_FILES) --profile stack up -d --build avalon-server
	@echo "avalon-server should now be reachable at http://127.0.0.1:8080 (Redis-backed rate limiting/concurrency ceiling enabled — 'make stack-up-no-redis' to skip it) — 'make stack-logs' to follow it, 'make stack-down' to stop."

stack-up-no-redis:
	@if [ ! -f .env ]; then \
		echo "no .env found — generating one from .env.compose.example"; \
		cp .env.compose.example .env; \
		sed -i.bak "s/^AVALON_NETWORK_ID=.*/AVALON_NETWORK_ID=avalon-stack-$$(python3 -c 'import secrets; print(secrets.token_hex(4))')/" .env && rm -f .env.bak; \
		echo "" >> .env; \
		echo "# Generated by 'make stack-up-no-redis' — unique to this deployment, do not share or commit." >> .env; \
		echo "AVALON_SETTLEMENT_SIGNING_KEY=$$(python3 -c 'import secrets; print(secrets.token_hex(32))')" >> .env; \
		chmod 600 .env; \
		DISCOVERED=$$(docker compose -f docker-compose.yml --profile stack run --rm discover-mirror-peers); \
		if [ -n "$$DISCOVERED" ]; then \
			echo "AVALON_MIRROR_PEERS=$$DISCOVERED" >> .env; \
			echo "discover-mirror-peers: configured AVALON_MIRROR_PEERS=$$DISCOVERED"; \
		fi; \
	fi
	docker compose -f docker-compose.yml --profile stack up -d --build postgres
	docker compose -f docker-compose.yml --profile stack run --rm migrate
	docker compose -f docker-compose.yml --profile stack up -d --build avalon-server
	@echo "avalon-server should now be reachable at http://127.0.0.1:8080 (no Redis — rate limiting/concurrency ceiling are per-process) — 'make stack-logs' to follow it, 'make stack-down' to stop."

# Always references both compose files regardless of which stack-up
# variant brought the stack up — `docker compose down`/`logs` no-ops
# cleanly for a service that was never started (e.g. redis, under
# stack-up-no-redis), so this never leaves an orphaned container behind
# either way.
stack-down:
	docker compose $(STACK_COMPOSE_FILES) --profile stack down

stack-logs:
	docker compose $(STACK_COMPOSE_FILES) --profile stack logs -f avalon-server

# --- JS/TS workspace (apps/hub, apps/mobile-hub, packages/ui) ---------------
# npm workspaces, declared in the repo-root package.json. Nothing here has
# been `npm install`ed yet in a verified environment — versions are pinned
# but unconfirmed against a real install.

web-install:
	npm install

hub-dev:
	npm run dev -w apps/hub

mobile-dev:
	npm run tauri dev -w apps/mobile-hub

storybook:
	npm run storybook -w packages/ui

web-build:
	npm run build -w packages/ui
	npm run build -w apps/hub
	npm run build -w apps/mobile-hub

web-lint:
	npm run lint -w apps/hub
	npm run lint -w apps/mobile-hub 2>/dev/null || true
	npm run lint -w packages/ui

web-test:
	npm run test -w apps/hub

# --- C# SDK (bindings/csharp) — flagship external SDK for game developers ---

csharp-build:
	cd bindings/csharp && dotnet build AvalonSdk.sln

csharp-test:
	cd bindings/csharp && dotnet test AvalonSdk.sln

# --- Ledger inspection -------------------------------------------------------

inspect-ledger:
	cargo run -p avalon-cli -- inspect-ledger

inspect-ledger-full:
	cargo run -p avalon-cli -- inspect-ledger-full

create-identity:
	cargo run -p avalon-cli -- create-identity

login:
	cargo run -p avalon-cli -- login $(IDENTITY_ID)

outbox-status:
	cargo run -p avalon-cli -- outbox-status

register-integrator:
	cargo run -p avalon-cli -- register-integrator --slug $(SLUG) --name $(NAME) --owner-name $(OWNER)

issue-achievement:
	cargo run -p avalon-cli -- issue-achievement --integrator $(INTEGRATOR) --achievement $(ACHIEVEMENT) --token $(TOKEN)

# --- Everything -------------------------------------------------------------

check-all: check web-lint web-test csharp-build csharp-test

clean-all: clean
	rm -rf node_modules apps/*/node_modules apps/*/dist packages/*/node_modules
	rm -rf apps/mobile-hub/src-tauri/target
	find bindings/csharp -type d \( -name bin -o -name obj \) -exec rm -rf {} +
