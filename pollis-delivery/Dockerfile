# Pollis MLS Delivery Service image.
#
# Build from the REPO ROOT (the whole cargo workspace must resolve):
#   docker build -f pollis-delivery/Dockerfile -t pollis-delivery .
#
# Run beside the LiveKit container, behind api.pollis.com (terminate TLS at a
# reverse proxy). Required env: TURSO_URL, TURSO_TOKEN. Optional: PORT (8788).
#
# Only pollis-delivery + its deps compile here — none of the desktop/media
# crates are in its dependency closure, so no media system libraries are needed.

FROM rust:1-bookworm AS build
# pkg-config/openssl for TLS deps; clang/libclang for libsql-ffi's bindgen;
# cmake/cc for native builds.
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev build-essential cmake clang libclang-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Bake the source git SHA into the binary (via `option_env!("GIT_SHA")`, exposed
# at `/version`) so a deploy can verify the new image is actually running.
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}
RUN cargo build --release -p pollis-delivery \
    && strip target/release/pollis-delivery

FROM debian:bookworm-slim AS runtime
# ca-certificates so libsql can TLS to Turso.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/pollis-delivery /usr/local/bin/pollis-delivery
# Cache-bust arg: bumping it yields a new image digest with no code change, which
# is the only way to force a Cloudflare Container to roll (e.g. to re-read a
# rotated Secrets Store value — CF does not roll on secret-only changes). Runtime
# no-op. Set via the container `image_vars.BUILD_NONCE` in the wrangler config.
ARG BUILD_NONCE=0
RUN echo "$BUILD_NONCE" > /etc/pollis-build-nonce
EXPOSE 8788
ENTRYPOINT ["/usr/local/bin/pollis-delivery"]
