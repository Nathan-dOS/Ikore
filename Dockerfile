# syntax=docker/dockerfile:1
ARG RUST_IMAGE=rust:1.98.0-bookworm
FROM ${RUST_IMAGE}

LABEL maintainer="Nathan <Nathan-dOS>"
LABEL description="Hard Vibe Code - Rust headless validation and testing environment"

ENV DEBIAN_FRONTEND=noninteractive
ENV CARGO_TERM_COLOR=always
ENV RUST_BACKTRACE=1
ENV PATH="/usr/local/cargo/bin:${PATH}"

# Install system compilation dependencies for C libraries (SQLite3, tree-sitter, OpenSSL, D-Bus)
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    cmake \
    libssl-dev \
    libclang-dev \
    libdbus-1-dev \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Pin rustup toolchain components matching rust-toolchain.toml (1.98.0)
ARG RUST_TOOLCHAIN=1.98.0
RUN rustup default ${RUST_TOOLCHAIN} && \
    rustup component add clippy rustfmt

WORKDIR /workspace

# Copy workspace configuration and vendorized dependencies
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY third_party ./third_party
COPY crates ./crates

# Default command executes headless test suite
CMD ["cargo", "test", "-p", "hvc-core", "-p", "hvc-editor", "-p", "hvc-test-support"]
