# The Shuffling Tome (mtg-scan) as a container: the web UI on port 8787,
# everything it keeps (SQLite database, settings, caches, an optional .env)
# in the /data volume. See compose.yaml and README.md ("Docker").

# ---- build: the web UI (WebAssembly), then the server with the UI embedded
FROM rust:1-bookworm AS build

# bindgen (Video4Linux bindings) needs libclang.
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown
ARG TRUNK_VERSION=0.22.0
RUN curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk

WORKDIR /src
COPY . .
RUN cd ui && trunk build --release
# Headless: no desktop window; PaddleOCR for OCR.
RUN cargo build --release --locked --no-default-features --features paddle-ocr
# The OCR models ship in the image, so it reads cards offline from the start.
RUN MTG_SCAN_MODELS_DIR=/models target/release/mtg-scan fetch-ocr-models

# ---- runtime
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# A fixed, unprivileged user. `video` lets it open a camera passed in with
# --device /dev/video0.
RUN useradd --system --uid 10001 --user-group --groups video --home-dir /data --no-create-home mtg \
    && mkdir /data \
    && chown mtg:mtg /data

COPY --from=build /src/target/release/mtg-scan /usr/local/bin/mtg-scan
COPY --from=build /models /opt/mtg-scan/models

ENV MTG_SCAN_DATA_DIR=/data \
    MTG_SCAN_CONFIG=/data/mtg-scan.toml \
    MTG_SCAN_CACHE_DIR=/data/cache \
    MTG_SCAN_MODELS_DIR=/opt/mtg-scan/models \
    MTG_SCAN_PORT=8787

# The working folder, so a /data/.env is read too.
WORKDIR /data
VOLUME /data
USER mtg
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD ["mtg-scan", "health"]
ENTRYPOINT ["mtg-scan"]
CMD ["ui", "--no-open"]
