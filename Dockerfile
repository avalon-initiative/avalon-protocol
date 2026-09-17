# Builds `avalon-server` and its `migrate` companion binary (issue #289) —
# the one-command hoster bring-up path's runtime image. Not yet
# multi-stage-cached for incremental rebuilds (no cargo-chef layer); a first
# correct build, not an optimized one — worth revisiting if `make stack-up`
# rebuild times become a real hoster complaint.
#
# IMPORTANT: `crates/server/src/bin/migrate.rs` resolves its migrations
# directory via `env!("CARGO_MANIFEST_DIR")` — a *compile-time* constant
# baked into the binary as the build machine's absolute path. The runtime
# stage below must therefore place `crates/server/db/migrations` at the
# exact same absolute path it occupied during the build (`WORKDIR` matches
# across both stages) — moving the compiled binary to a conventional
# `/app` layout without also relocating `db/` under it would make `migrate`
# fail to find its own migrations at runtime.
FROM rust:1-slim-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /usr/src/avalon-protocol
COPY . .
RUN cargo build --release -p avalon-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /usr/src/avalon-protocol
COPY --from=builder /usr/src/avalon-protocol/target/release/avalon-server /usr/local/bin/avalon-server
COPY --from=builder /usr/src/avalon-protocol/target/release/migrate /usr/local/bin/migrate
COPY --from=builder /usr/src/avalon-protocol/crates/server/db ./crates/server/db

EXPOSE 8080
CMD ["avalon-server"]
