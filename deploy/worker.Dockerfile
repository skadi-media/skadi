# Multi-stage build for the `skadi-downloader-worker` binary (SKADI-I-0013).
#
# The worker embeds librqbit and is DELIBERATELY excluded from the main
# workspace (see root Cargo.toml), so it is built with an explicit
# `--manifest-path`. It links `skadi-store` (a path dependency into the
# workspace), so the build context is the repo root — same as the daemon's
# Dockerfile. No web UI / trunk here; it serves no HTTP.
#
# Runtime is slim Debian with just the libraries diesel links against
# (libpq5 + libsqlite3-0 — both backends compiled in, selected at runtime by
# SKADI_DATABASE_URL). No toolchain ships at runtime.

# --- build stage ---------------------------------------------------------
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsqlite3-dev \
        libpq-dev \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# The worker is its own (excluded) package, so its target dir is crate-local.
RUN cargo build --release --manifest-path crates/skadi-downloader-worker/Cargo.toml

# --- runtime stage -------------------------------------------------------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsqlite3-0 \
        libpq5 \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/* \
    # Explicit gid (SKADI-T-0487): `useradd --system` picks a gid from the
    # low system range — 999 on bookworm today — so the group the container
    # runs as was an accident of the base image. The compose files default
    # PGID to this value, so leaving it implicit means a base-image bump
    # could move the group out from under a library full of files.
    && groupadd --system --gid 999 skadi \
    && useradd --system --uid 1000 --gid 999 --create-home skadi

COPY --from=builder \
    /src/crates/skadi-downloader-worker/target/release/skadi-downloader-worker \
    /usr/local/bin/skadi-downloader-worker

USER skadi
WORKDIR /home/skadi

# Defaults; the compose file overrides DOWNLOAD_DIR / DATABASE_URL / WORKER_ID.
# Under the skadi-owned library root's downloads/ tree (SKADI-T-0302/0305).
ENV SKADI_WORKER_DOWNLOAD_DIR=/mnt/storage/skadi/downloads/complete

# jemalloc tuning (SKADI-T-0392; the binary's global allocator is
# tikv-jemallocator, whose env prefix is `_RJEM_`). A handful of arenas shared by
# all threads instead of one per thread, and a background thread that purges
# freed piece buffers on the decay clock even when the thread that freed them is
# parked forever in tokio's blocking pool. Target: < 1 GiB steady state on the
# 4 GB DS1821+.
ENV _RJEM_MALLOC_CONF=narenas:4,background_thread:true,dirty_decay_ms:10000,muzzy_decay_ms:0

ENTRYPOINT ["/usr/local/bin/skadi-downloader-worker"]
