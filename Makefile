-include .env
export

RUN_DIR := _running
PID_DIR := $(RUN_DIR)/pids
LOG_DIR := $(RUN_DIR)/logs
PID_FILE := $(PID_DIR)/avalon-server.pid
LOG_FILE := $(LOG_DIR)/avalon-server.log

.PHONY: help \
	build run start stop restart status test test-live test-live-raw load-test fmt fmt-check lint check clean \
	openapi openapi-check openapi-version-check check-trust-anchors \
	migrate migrate-down db-reset \
	stack-up stack-up-no-redis stack-down stack-logs \
	inspect-ledger inspect-ledger-full create-identity login outbox-status \
	register-integrator issue-achievement \
	check-all clean-all

help:
	@echo "avalon-protocol — local dev commands"
	@echo ""
	@echo "Rust workspace (crates/) — protocol, chain, indexer, server, cli"
	@echo "  (the Rust reference SDK lives in the avalon-sdks repo — 'cargo build/test' here reach it via a git dependency, not a workspace member)"
	@echo "  make build         cargo build --workspace"
	@echo "  make run           run avalon-server in the foreground"
	@echo "  make start         run avalon-server in the background (pid/log under $(RUN_DIR)/)"
	@echo "  make stop          stop what 'make start' started"
	@echo "  make restart       stop, then start"
	@echo "  make status        report whether the background process is running"
	@echo "  make test          cargo test --workspace"
	@echo "  make test-live     scripts/live-tests.sh: --ignored suite against private, isolated servers (GROUPS=... to pick)"
	@echo "  make test-live-raw cargo test --workspace -- --ignored (needs a matching server already running)"
	@echo "  make load-test     scripts/load-tests.sh: load scenarios against private, isolated servers (SCENARIOS=... to pick, LOAD_SCALE=full for a real run)"
	@echo "  make fmt           cargo fmt --all"
	@echo "  make fmt-check     cargo fmt --all -- --check"
	@echo "  make lint          cargo clippy --workspace --all-targets -- -D warnings"
	@echo "  make openapi       regenerate docs/generated/openapi.json from server's annotated routes"
	@echo "  make openapi-check fail if docs/generated/openapi.json is stale relative to the real routes"
	@echo "  make check-trust-anchors  fail if the README trusted-networks table drifts from docs/trusted-networks.json"
	@echo "  make openapi-version-check fail if the schema's shape changed vs. main without a version bump"
	@echo "  (TS/C# SDK type regeneration lives in avalon-sdks with the SDKs themselves)"
	@echo "  make migrate       apply pending db/migrations/ (up)"
	@echo "  make migrate-down  revert the most recently applied migration"
	@echo "  make db-reset      wipe the database (drop+recreate public schema) and reapply all migrations"
	@echo "  make stack-up          Docker Compose bring-up: postgres + redis + avalon-server, migrated and started, no Rust/Node toolchain needed"
	@echo "  make stack-up-no-redis same as stack-up, without the Redis-backed rate limit/concurrency ceiling"
	@echo "  make stack-down        stop what 'make stack-up'/'make stack-up-no-redis' started"
	@echo "  make stack-logs        follow avalon-server's logs inside the compose stack"
	@echo "  make check         fmt-check + lint + test — what CI runs"
	@echo "  make clean         remove Rust build artifacts and PID/log files"
	@echo ""
	@echo "  (C# and TypeScript SDKs live in the avalon-sdks repo now: dotnet/npm build+test run there)"
	@echo ""
	@echo "  make inspect-ledger        pretty-print the hash-chained ledger (avalon-cli)"
	@echo "  make inspect-ledger-full   same, plus each entry's actual payload"
	@echo "  make create-identity  register a new self-custodied (passkey) identity via avalon-cli"
	@echo "  make login IDENTITY_ID=<uuid>  log in an identity create-identity saved locally, print a session token"
	@echo "  make outbox-status    pending/oldest-pending count for the settlement outbox"
	@echo "  make register-integrator SLUG=<slug> NAME=<name> OWNER=<owner>  register a test integrator, save its key locally"
	@echo "  make issue-achievement INTEGRATOR=<slug> ACHIEVEMENT=<key> TOKEN=<session-token>  issue an already-defined achievement to the identity behind TOKEN"
	@echo ""
	@echo "  make check-all     check (Rust)"
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

# The `--ignored` suite, run against servers this script starts itself on
# 127.0.0.1 with throwaway schemas (see scripts/live-tests.sh). Never touches a
# running node.
test-live:
	scripts/live-tests.sh $(GROUPS)

# Load scenarios against servers the script starts itself on 127.0.0.1 with
# throwaway schemas (see scripts/load-tests.sh). The generator refuses
# non-loopback targets.
load-test:
	scripts/load-tests.sh $(SCENARIOS)

# Plain `--ignored` run against whatever server AVALON_SERVER_URL points at;
# the multi-process tests fail without their extra processes and env vars.
test-live-raw:
	cargo test --workspace -- --ignored

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

# The Rust SDK's own examples/rustdoc checks
# (formerly `sdk-examples`/`sdk-doc`) moved with it into the
# avalon-sdks repo — `cargo build -p avalon-sdk --examples`/`cargo doc -p
# avalon-sdk` don't work here anymore since it's a git dependency, not a
# workspace member. Run them from avalon-sdks directly.

# `docs/generated/openapi.json` is a generated artifact, not
# hand-maintained — regenerate it whenever an in-scope route/type changes.
# A real bootstrapping bug here is still guarded against (even
# though `avalon-sdk`'s own build.rs now reads its own
# vendored copy in avalon-sdks, no longer this file directly): a plain
# `cargo run ... > docs/generated/openapi.json` redirect truncates the file
# the instant the shell opens it, before cargo even starts — this repo's
# own `dump-openapi` binary reads this same file indirectly via
# avalon-server's build, so writing to a scratch file first and moving it
# into place only after a successful run avoids ever truncating a file a
# concurrent build might be reading mid-regeneration.
openapi:
	cargo run -p avalon-server --bin dump-openapi > /tmp/openapi.generated.json
	mv /tmp/openapi.generated.json docs/generated/openapi.json

# CI check: regenerate into a scratch file and diff against the checked-in
# one, so a route/type change that forgot to re-run `make openapi` fails
# the build instead of silently drifting.
openapi-check:
	cargo run -p avalon-server --bin dump-openapi > /tmp/openapi.generated.json
	diff docs/generated/openapi.json /tmp/openapi.generated.json || \
		(echo "docs/generated/openapi.json is stale — run 'make openapi' and commit the result" && exit 1)

# Issue #735: openapi-check above only catches staleness (checked-in file
# doesn't match the real routes) — this separately requires a version bump
# whenever the schema's shape actually changed relative to main, so SDKs'
# embedded OPENAPI_SCHEMA_VERSION constants stay a real drift signal.
openapi-version-check:
	./scripts/check-openapi-version.sh

# The TypeScript and C# SDKs' own type-regeneration steps (formerly
# ts-sdk-types/ts-sdk-types-check and csharp-sdk-types/csharp-sdk-types-check)
# live in the avalon-sdks repo now — run them from there directly.

check: fmt-check lint test openapi-check openapi-version-check check-trust-anchors

check-trust-anchors:
	node scripts/check-trust-anchors.mjs

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

# --- One-command hoster bring-up --------------------------------------------
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

check-all: check

clean-all: clean
