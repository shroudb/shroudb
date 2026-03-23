FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y musl-tools && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build

# Copy commons (sibling directory — CI checks it out, local dev has it adjacent)
COPY commons/ /commons/

# Copy keyva source
COPY keyva/ /build/

RUN cargo build --release --target x86_64-unknown-linux-musl \
    --bin keyva \
    --bin keyva-auth \
    --bin keyva-cli

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/keyva /keyva
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/keyva-auth /keyva-auth
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/keyva-cli /keyva-cli

USER nonroot:nonroot
EXPOSE 6399 8080 4001

ENTRYPOINT ["/keyva"]
