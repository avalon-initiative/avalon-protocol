-include .env
export

RUN_DIR := _running
PID_DIR := $(RUN_DIR)/pids
LOG_DIR := $(RUN_DIR)/logs
PID_FILE := $(PID_DIR)/avalon-server.pid
LOG_FILE := $(LOG_DIR)/avalon-server.log

.PHONY: help \
	build run start stop restart status test test-live test-rocksdb fmt fmt-check lint check clean \
	migrate migrate-down db-reset \
	web-install hub-dev mobile-dev storybook web-build web-lint web-test \
	csharp-build csharp-test \
	inspect-ledger inspect-ledger-full create-identity login register-game outbox-status \
	cli-prod-build \
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
	@echo "  make test-rocksdb  cargo test -p avalon-chain --features rocksdb-backend (needs no live infra — a temp dir, unlike test-live)"
	@echo "  make fmt           cargo fmt --all"
	@echo "  make fmt-check     cargo fmt --all -- --check"
	@echo "  make lint          cargo clippy --workspace --all-targets -- -D warnings"
	@echo "  make migrate       apply pending db/migrations/ (up)"
	@echo "  make migrate-down  revert the most recently applied migration"
	@echo "  make db-reset      wipe the database (drop+recreate public schema) and reapply all migrations"
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
	@echo "  make register-game SLUG=<slug> NAME=<name> DEVELOPER=<dev>  register a game, mint its signing key"
	@echo "  make outbox-status    pending/oldest-pending count for the settlement outbox (issue #71)"
	@echo ""
	@echo "  All of the above (except inspect-ledger/inspect-ledger-full/outbox-status)"
	@echo "  need the default 'dev-tools' cargo feature (on by default) — see cli-prod-build."
	@echo "  make cli-prod-build   builds avalon-cli WITHOUT dev-tools (issue #175): no"
	@echo "                        create-identity/login/register-game in the binary at all,"
	@echo "                        just inspect-ledger/inspect-ledger-full/outbox-status."
	@echo ""
	@echo "  Every avalon-server/avalon-cli command above needs AVALON_NETWORK_ID set in"
	@echo "  .env (issue #173) — see .env.example. A fresh database's genesis is created"
	@echo "  from that value on first boot; a later mismatch refuses to start, on purpose."
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

# RocksDbSettlementProvider's own suite (issue #178) — behind the off-by-
# default `rocksdb-backend` feature, but unlike test-live, needs no live
# infra at all (a temp directory is enough), so it's a real `test`, not
# `--ignored`, once the feature is on.
test-rocksdb:
	cargo test -p avalon-chain --features rocksdb-backend

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

check: fmt-check lint test

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

register-game:
	cargo run -p avalon-cli -- register-game --slug $(SLUG) --name "$(NAME)" --developer "$(DEVELOPER)" $(if $(CAPABILITY),--capability $(CAPABILITY),) $(if $(SERVER),--server $(SERVER),)

outbox-status:
	cargo run -p avalon-cli -- outbox-status

# Issue #175: everything above (except inspect-ledger/inspect-ledger-full/
# outbox-status) only exists in a `dev-tools`-featured build — the default.
# This is what a build meant to run anywhere near a real deployment should
# actually use: create-identity/login/register-game are absent from the
# resulting binary, not merely refused at runtime.
cli-prod-build:
	cargo build -p avalon-cli --release --no-default-features

# --- Everything -------------------------------------------------------------

check-all: check web-lint web-test csharp-build csharp-test

clean-all: clean
	rm -rf node_modules apps/*/node_modules apps/*/dist packages/*/node_modules
	rm -rf apps/mobile-hub/src-tauri/target
	find bindings/csharp -type d \( -name bin -o -name obj \) -exec rm -rf {} +
