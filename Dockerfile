FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y musl-tools && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY commons/ /commons/
COPY keyva/ /build/

RUN rustup target add x86_64-unknown-linux-musl

RUN cargo build --release --target x86_64-unknown-linux-musl \
    -p keyva -p keyva-auth -p keyva-cli

# --- keyva: credential management server ---
FROM gcr.io/distroless/static-debian12:nonroot AS keyva
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/keyva /keyva
USER nonroot:nonroot
EXPOSE 6399 8080
ENTRYPOINT ["/keyva"]

# --- keyva-auth: standalone auth server ---
FROM gcr.io/distroless/static-debian12:nonroot AS keyva-auth
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/keyva-auth /keyva-auth
USER nonroot:nonroot
EXPOSE 4001
ENTRYPOINT ["/keyva-auth"]

# --- keyva-cli: command-line client ---
FROM gcr.io/distroless/static-debian12:nonroot AS keyva-cli
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/keyva-cli /keyva-cli
USER nonroot:nonroot
ENTRYPOINT ["/keyva-cli"]
