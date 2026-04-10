FROM rust:slim-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
      cmake build-essential pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install nightly --profile minimal \
    && cargo install cargo-fuzz --locked

WORKDIR /build
COPY . /build/

ARG REGISTRY_TOKEN
RUN mkdir -p /root/.cargo && \
    printf '[source.crates-io]\nreplace-with = "shroudb-cratesio"\n\n[source.shroudb-cratesio]\nregistry = "sparse+https://crates.shroudb.dev/api/v1/cratesio/"\n\n[registries.shroudb-cratesio]\nindex = "sparse+https://crates.shroudb.dev/api/v1/cratesio/"\ncredential-provider = ["cargo:token"]\n\n[registries.shroudb]\nindex = "sparse+https://crates.shroudb.dev/api/v1/crates/"\ncredential-provider = ["cargo:token"]\n' > /root/.cargo/config.toml

ENV CARGO_REGISTRIES_SHROUDB_CRATESIO_TOKEN=${REGISTRY_TOKEN} \
    CARGO_REGISTRIES_SHROUDB_TOKEN=${REGISTRY_TOKEN}

WORKDIR /build/fuzz
RUN cargo +nightly fuzz build

ENTRYPOINT ["cargo", "+nightly", "fuzz", "run"]
CMD ["resp3_parser", "--", "-max_total_time=60", "-max_len=4096"]
