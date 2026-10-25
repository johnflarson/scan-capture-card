# tome-collector as a container: listens on 8790, keeps its SQLite file in
# the /data volume. Build from the repository's root:
#   docker build -f collector/Dockerfile -t tome-collector .
# See collector/README.md.

FROM rust:1-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p tome-collector

FROM debian:trixie-slim
# Links the image to the repository on GitHub.
LABEL org.opencontainers.image.source="https://github.com/johnflarson/scan-capture-card" \
      org.opencontainers.image.description="The Shuffling Tome usage statistics collector"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10002 --user-group --home-dir /data --no-create-home collector \
    && mkdir /data \
    && chown collector:collector /data
COPY --from=build /src/target/release/tome-collector /usr/local/bin/tome-collector
ENV TOME_COLLECTOR_BIND=0.0.0.0:8790 \
    TOME_COLLECTOR_DB=/data/collector.db
WORKDIR /data
VOLUME /data
USER collector
EXPOSE 8790
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
    CMD ["tome-collector", "health"]
ENTRYPOINT ["tome-collector"]
