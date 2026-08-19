ARG NODE_IMAGE=node:22-bookworm
FROM ${NODE_IMAGE}

ARG RUST_TOOLCHAIN=1.88.0
ENV DEBIAN_FRONTEND=noninteractive
ENV PATH="/root/.cargo/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    curl \
    file \
    libssl-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    libwebkit2gtk-4.1-dev \
    libjavascriptcoregtk-4.1-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --default-toolchain ${RUST_TOOLCHAIN}

WORKDIR /workspace

COPY package.json package-lock.json ./
COPY src-tauri/Cargo.toml ./src-tauri/Cargo.toml
COPY src-tauri/build.rs ./src-tauri/build.rs

RUN mkdir -p src-tauri/src && echo "fn main() {}" > src-tauri/src/main.rs
RUN npm ci
RUN cargo fetch --manifest-path src-tauri/Cargo.toml

COPY . .

EXPOSE 5173
CMD ["npm", "run", "dev", "--", "--host", "0.0.0.0", "--port", "5173"]
