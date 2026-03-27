# Cross-compilation images — selected by TARGETARCH (set automatically by buildx)
ARG TARGETARCH=amd64
FROM --platform=$BUILDPLATFORM messense/rust-musl-cross:x86_64-musl AS cross-amd64
FROM --platform=$BUILDPLATFORM messense/rust-musl-cross:aarch64-musl AS cross-arm64
FROM cross-${TARGETARCH} AS builder

WORKDIR /build
COPY . .

ARG TARGETARCH
ARG REGISTRY_TOKEN
RUN mkdir -p /root/.cargo && \
    printf '[source.crates-io]\nreplace-with = "shroudb-cratesio"\n\n[source.shroudb-cratesio]\nregistry = "sparse+https://crates.shroudb.dev/api/v1/cratesio/"\n\n[registries.shroudb-cratesio]\nindex = "sparse+https://crates.shroudb.dev/api/v1/cratesio/"\ncredential-provider = ["cargo:token"]\n\n[registries.shroudb]\nindex = "sparse+https://crates.shroudb.dev/api/v1/crates/"\ncredential-provider = ["cargo:token"]\n' > /root/.cargo/config.toml && \
    RUST_TARGET=$(if [ "$TARGETARCH" = "arm64" ]; then echo "aarch64-unknown-linux-musl"; else echo "x86_64-unknown-linux-musl"; fi) && \
    CARGO_REGISTRIES_SHROUDB_CRATESIO_TOKEN="$REGISTRY_TOKEN" \
    CARGO_REGISTRIES_SHROUDB_TOKEN="$REGISTRY_TOKEN" \
    cargo build --release --target "$RUST_TARGET" \
    -p shroudb -p shroudb-cli && \
    mkdir -p /out && \
    cp "target/$RUST_TARGET/release/shroudb" /out/ && \
    cp "target/$RUST_TARGET/release/shroudb-cli" /out/

# --- shroudb: credential management server ---
FROM alpine:3.21 AS shroudb
RUN adduser -D -u 65532 shroudb && \
    mkdir /data && chown shroudb:shroudb /data
LABEL org.opencontainers.image.title="ShrouDB" \
      org.opencontainers.image.description="Encrypted credential vault with key rotation, RESP3 protocol, and WAL storage" \
      org.opencontainers.image.vendor="ShrouDB" \
      org.opencontainers.image.url="https://github.com/shroudb/shroudb" \
      org.opencontainers.image.source="https://github.com/shroudb/shroudb" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=builder /out/shroudb /shroudb
VOLUME /data
WORKDIR /data
USER shroudb
EXPOSE 6399 9090
ENTRYPOINT ["/shroudb"]

# --- shroudb-cli: command-line client ---
FROM alpine:3.21 AS shroudb-cli
RUN adduser -D -u 65532 shroudb
LABEL org.opencontainers.image.title="ShrouDB CLI" \
      org.opencontainers.image.description="Command-line client for ShrouDB credential vault" \
      org.opencontainers.image.vendor="ShrouDB" \
      org.opencontainers.image.url="https://github.com/shroudb/shroudb" \
      org.opencontainers.image.source="https://github.com/shroudb/shroudb" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=builder /out/shroudb-cli /shroudb-cli
USER shroudb
ENTRYPOINT ["/shroudb-cli"]
