# syntax=docker/dockerfile:1
#
# PLACEHOLDER image for the headless daemon (MASTER-PLAN §4.5). The daemon
# does not exist yet, so this builds and runs `whoosh-labserver` instead; swap
# the package and entrypoint when it lands. See README.md.
#
# Build from the repository root:
#   docker build -f packaging/docker/Dockerfile -t whoosh-labserver .
#
# Base images are pinned by multi-arch index digest; keep the Rust version in
# step with rust-toolchain.toml.

FROM rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS build

WORKDIR /src
# Use the image's toolchain as-is: rust-toolchain.toml would also pull
# rustfmt and clippy, which a release build does not need.
ENV RUSTUP_TOOLCHAIN=1.99.1 \
    CARGO_INCREMENTAL=0 \
    CARGO_TERM_COLOR=never

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p whoosh-labserver \
 && install -D -m 0755 target/release/whoosh-labserver /out/whoosh-labserver

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

LABEL org.opencontainers.image.title="whoosh-labserver" \
      org.opencontainers.image.description="Fault-injecting HTTP test server (stand-in for the whoosh daemon image)" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/coderzonebd/whoosh-download-manager"

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 whoosh \
 && useradd --system --uid 10001 --gid whoosh --home-dir /var/lib/whoosh --create-home \
      --shell /usr/sbin/nologin whoosh

COPY --from=build /out/whoosh-labserver /usr/local/bin/whoosh-labserver

USER 10001:10001
WORKDIR /var/lib/whoosh
EXPOSE 18080 18443
ENTRYPOINT ["/usr/local/bin/whoosh-labserver"]
# Listen on all interfaces inside the container; publish ports with -p.
CMD ["--bind", "0.0.0.0", "--port", "18080", "--tls-port", "18443"]
