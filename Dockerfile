# syntax=docker/dockerfile:1
ARG RUST_IMAGE=rust:1.98.0-bookworm
FROM ${RUST_IMAGE}

LABEL org.opencontainers.image.title="hard-vibe-code-headless"
LABEL org.opencontainers.image.description="Linux headless harness for HVC contracts, terminal, core, editor and test-support"
LABEL org.opencontainers.image.source="https://github.com/Nathan-dOS/HARD-VC"

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=always \
    RUST_BACKTRACE=1

# System build dependencies for the C libraries (SQLite3, tree-sitter, OpenSSL, D-Bus)
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config cmake libssl-dev libclang-dev libdbus-1-dev \
        curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Keep in step with rust-toolchain.toml
ARG RUST_TOOLCHAIN=1.98.0
RUN rustup default ${RUST_TOOLCHAIN} && rustup component add clippy rustfmt

WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml ./
COPY .cargo ./.cargo
COPY third_party ./third_party
COPY crates ./crates

# Check the lockfile and fetch registry sources into BuildKit's cache. Cache
# mounts are not part of the image: containers get the registry from the
# compose volumes (or .ci-cache/ in CI).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo fetch --locked

CMD ["cargo", "test-headless"]
