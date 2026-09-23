# Multi-stage build for the `skadi` daemon/CLI binary (SKADI-T-0064).
#
# Builder compiles the workspace's `skadi` bin in release mode AND builds the
# Leptos web UI (SKADI-T-0068) with trunk, embedding it into the binary via the
# `embed-ui` feature. The runtime stage is a slim Debian with just the shared
# libraries diesel links against (libsqlite3 + libpq — both backends compiled
# in, selected at runtime by SKADI_DATABASE_URL). No toolchain ships at runtime.

# --- ffprobe stage -------------------------------------------------------
# A minimal, statically-linked `ffprobe` for ONE field the pure-Rust probe cannot
# reach: **Dolby Vision** (SKADI-T-0569). DV is signalled per block rather than in
# the track header, so no header walk can see it — SKADI-T-0543 documented it as
# out of reach for exactly that reason.
#
# **Built from source rather than installed**, because Debian's `ffmpeg` package
# is a catastrophic way to obtain `ffprobe`: it links 213 shared libraries and
# costs **+376 MB** (97 MB base → 473 MB), dominated by things a header reader has
# no use for — libllvm15 (109 MB), libicu72 (36 MB), libflite1 (27 MB, a speech
# synthesiser), libgl1-mesa-dri (23 MB). Deleting the ffmpeg/ffplay binaries does
# not help; the libraries are the weight.
#
# This build is **2.77 MB, fully static**, with only the two demuxers skadi probes
# and the parsers that expose DV side data. No encoders, no filters, no network,
# no external codecs — so the transcoder is not merely unused here, it does not
# exist in the image. SKADI-T-0139 (MP3→M4B) stays a separate, unmade decision.
FROM debian:bookworm-slim AS ffprobe-build
ARG FFMPEG_VERSION=7.1.1
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential curl ca-certificates xz-utils pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /ff
RUN curl -fsSL "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" -o ff.tar.xz \
    && tar xf ff.tar.xz --strip-components=1 \
    && rm ff.tar.xz
# The demuxer list decides what the library scan can SEE (SKADI-T-0583).
#
# It was originally scoped to the containers the in-process readers already
# handle, which is exactly backwards: ffprobe is only ever consulted for the ones
# they cannot read. Measured on the live library that gap hid 68 files — 58
# .m2ts, 8 .avi, 2 .ts — and a 54 GB Blu-ray remux among them is the single
# strongest repack candidate in the collection.
#
# Parsers matter for mpegts in particular: its stream types are not in a header
# to read, so without one ffprobe reports the container and nothing about what is
# inside it. `dca` and `ac3` are here to *identify* DTS and AC-3 tracks, which is
# the most important thing the streaming report says.
RUN ./configure \
        --disable-everything \
        --disable-doc --disable-htmlpages --disable-manpages --disable-podpages --disable-txtpages \
        --disable-network --disable-autodetect --disable-iconv --disable-alsa \
        --disable-sdl2 --disable-xlib --disable-zlib --disable-bzlib --disable-lzma \
        --disable-ffmpeg --disable-ffplay --enable-ffprobe \
        --enable-demuxer=matroska,mov,mp3,flac,ogg,wav,mpegts,mpegtsraw,avi,mpeg,asf,flv \
        --enable-parser=hevc,h264,aac,mpegaudio,flac,opus,vorbis,av1,ac3,dca,mpegvideo,mpeg4video,vc1 \
        --enable-protocol=file \
        --enable-static --disable-shared \
        --extra-ldflags=-static \
    && make -j"$(nproc)" ffprobe \
    && strip ffprobe \
    # Fail the build rather than ship something that cannot run: static linkage is
    # the whole point, since the runtime stage carries none of these libraries.
    && ./ffprobe -version > /dev/null \
    && ! ldd ffprobe 2>&1 | grep -q "=>"

# --- build stage ---------------------------------------------------------
FROM rust:1-bookworm AS builder

# Pinned for reproducibility; bump deliberately. Matches the dev toolchain.
ARG TRUNK_VERSION=0.21.14

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsqlite3-dev \
        libpq-dev \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Web UI toolchain: the wasm target + trunk (prebuilt binary for the build
# arch). trunk auto-fetches the matching wasm-bindgen + wasm-opt at build time.
RUN rustup target add wasm32-unknown-unknown \
    && arch="$(uname -m)" \
    && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz" \
        | tar -xz -C /usr/local/bin trunk \
    && trunk --version

WORKDIR /src
COPY . .

# 1) Build the UI bundle into crates/skadi-web/dist (excluded from the workspace;
#    a stale host dist can't leak in — see .dockerignore).
RUN cd crates/skadi-web && trunk build --release
# 2) Compile the daemon with the UI embedded (rust-embed reads ../skadi-web/dist
#    at compile time, so this MUST run after the trunk build above).
RUN cargo build --release -p skadi-cli --features embed-ui

# --- runtime stage -------------------------------------------------------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        libsqlite3-0 \
        libpq5 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    # Explicit gid (SKADI-T-0487): `useradd --system` picks a gid from the
    # low system range — 999 on bookworm today — so the group the container
    # runs as was an accident of the base image. The compose files default
    # PGID to this value, so leaving it implicit means a base-image bump
    # could move the group out from under a library full of files.
    && groupadd --system --gid 999 skadi \
    && useradd --system --uid 1000 --gid 999 --create-home skadi

COPY --from=builder /src/target/release/skadi /usr/local/bin/skadi
# 2.77 MB, static, no runtime libraries needed (SKADI-T-0569).
COPY --from=ffprobe-build /ff/ffprobe /usr/local/bin/ffprobe

# Pre-create the cardigann definitions dir owned by `skadi`, so a FRESH named
# volume mounted at /data/definitions inherits skadi's ownership (Docker copies
# the image dir's owner to an empty volume). Without this the volume is root-owned
# and the daemon (uid 1000) can't seed its bundled definitions.
RUN mkdir -p /data/definitions && chown -R skadi:skadi /data

USER skadi
WORKDIR /home/skadi

# Inside a container we must bind beyond loopback for the port mapping to work.
ENV SKADI_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/skadi"]
CMD ["run"]
