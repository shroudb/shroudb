FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y musl-tools && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

RUN rustup target add x86_64-unknown-linux-musl

RUN --mount=type=secret,id=git_auth,env=GIT_AUTH_URL \
    if [ -n "$GIT_AUTH_URL" ]; then git config --global url."$GIT_AUTH_URL".insteadOf "https://github.com/"; fi && \
    cargo build --release --target x86_64-unknown-linux-musl \
    -p shroudb -p shroudb-cli

# --- shroudb: credential management server ---
FROM gcr.io/distroless/static-debian12:nonroot AS shroudb
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/shroudb /shroudb
USER nonroot:nonroot
EXPOSE 6399
ENTRYPOINT ["/shroudb"]

# --- shroudb-cli: command-line client ---
FROM gcr.io/distroless/static-debian12:nonroot AS shroudb-cli
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/shroudb-cli /shroudb-cli
USER nonroot:nonroot
ENTRYPOINT ["/shroudb-cli"]
