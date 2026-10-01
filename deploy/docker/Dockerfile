# Stein node image: validator or follower, chosen by STEIN_ROLE at run time.
# Built from source on Linux: no prebuilt objects reach a node (SPEC §9).
# The compiler matches rust-toolchain.toml; scripts/check-toolchain-pin.sh
# enforces it.
FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
# aws-lc-sys builds AWS-LC from source and wants CMake.
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Empty for standard images. stein-crypto/fn-dsa-experimental builds the
# FN-DSA test-network image (SPEC §3.5); never use that for anything else.
ARG CARGO_FEATURES=""
RUN cargo build --release --locked ${CARGO_FEATURES:+--features "$CARGO_FEATURES"} --bin stein-node --bin stein

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
# curl only for the health check. TLS roots are compiled into the binaries.
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --create-home stein
COPY --from=build /src/target/release/stein-node /src/target/release/stein /usr/local/bin/
COPY deploy/docker/entrypoint.sh /usr/local/bin/stein-entrypoint
ENV STEIN_DATA_DIR=/data \
    STEIN_RPC=0.0.0.0:8645 \
    STEIN_ROLE=validator
USER stein
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8645
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s CMD curl -fsS http://127.0.0.1:8645/status || exit 1
ENTRYPOINT ["stein-entrypoint"]
