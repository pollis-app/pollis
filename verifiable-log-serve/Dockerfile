# Transparency verification server image: builds the `serve` + `builder` binaries
# from the workspace and runs `serve` over a static tree built at startup.
#
# Build from the REPO ROOT (the whole cargo workspace must resolve):
#   docker build -f verifiable-log-serve/Dockerfile -t pollis-transparency .
#
# Only the verifiable-log* crates compile here — none of the desktop/media crates
# (electron, livekit, libwebrtc) are in their dependency closure, so no media
# system libraries are needed.

FROM rust:1-bookworm AS build
# pkg-config/openssl for TLS deps; clang/libclang for libsql-ffi's bindgen; cmake/cc for native builds.
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev build-essential cmake clang libclang-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p verifiable-log-serve -p verifiable-log-builder \
    && strip target/release/serve target/release/builder

FROM debian:bookworm-slim AS runtime
# ca-certificates so the builder can TLS to Turso.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/serve   /usr/local/bin/serve
COPY --from=build /src/target/release/builder /usr/local/bin/builder
COPY verifiable-log-serve/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh
# The public read API. Terminate TLS with a reverse proxy in front in production.
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
