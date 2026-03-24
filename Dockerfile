# Cross-compilation images — selected by TARGETARCH (set automatically by buildx)
ARG TARGETARCH=amd64
FROM --platform=$BUILDPLATFORM messense/rust-musl-cross:x86_64-musl AS cross-amd64
FROM --platform=$BUILDPLATFORM messense/rust-musl-cross:aarch64-musl AS cross-arm64
FROM cross-${TARGETARCH} AS builder

WORKDIR /build
COPY . .

ARG TARGETARCH
RUN --mount=type=secret,id=git_auth,env=GIT_AUTH_URL \
    if [ -n "$GIT_AUTH_URL" ]; then git config --global url."$GIT_AUTH_URL".insteadOf "https://github.com/"; fi && \
    RUST_TARGET=$(if [ "$TARGETARCH" = "arm64" ]; then echo "aarch64-unknown-linux-musl"; else echo "x86_64-unknown-linux-musl"; fi) && \
    cargo build --release --target "$RUST_TARGET" \
    -p shroudb -p shroudb-cli && \
    mkdir -p /out && \
    cp "target/$RUST_TARGET/release/shroudb" /out/ && \
    cp "target/$RUST_TARGET/release/shroudb-cli" /out/

# --- shroudb: credential management server ---
FROM gcr.io/distroless/static-debian12:nonroot AS shroudb
COPY --from=builder /out/shroudb /shroudb
USER nonroot:nonroot
EXPOSE 6399 9090
ENTRYPOINT ["/shroudb"]

# --- shroudb-cli: command-line client ---
FROM gcr.io/distroless/static-debian12:nonroot AS shroudb-cli
COPY --from=builder /out/shroudb-cli /shroudb-cli
USER nonroot:nonroot
ENTRYPOINT ["/shroudb-cli"]
