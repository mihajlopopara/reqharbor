# ReqHarbor dev/CI toolchain: Rust stable + Node 20 + Tauri 2 Linux build deps.
FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev \
      libssl-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev pkg-config build-essential \
      file curl ca-certificates xz-utils xvfb \
    && rm -rf /var/lib/apt/lists/*

ARG NODE_VERSION=22.21.1
RUN arch=$(dpkg --print-architecture | sed 's/amd64/x64/') \
    && curl -fsSL https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${arch}.tar.xz \
       | tar -xJ -C /usr/local --strip-components=1 \
    && npm install -g pnpm@9 \
    && rustup component add clippy rustfmt

# Headless Chromium system libs for Playwright UI tests; browsers live in a named volume.
RUN npx -y playwright@latest install-deps chromium && rm -rf /var/lib/apt/lists/* /root/.npm

# Real-app E2E: WebKitWebDriver + tauri-driver (drive the Tauri app under xvfb).
RUN apt-get update && apt-get install -y --no-install-recommends webkit2gtk-driver \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install tauri-driver --locked && rm -rf /usr/local/cargo/registry

ENV CARGO_TARGET_DIR=/cache/target CARGO_HOME=/usr/local/cargo PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
WORKDIR /app
