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
#
# Layered with cargo-chef so third-party dependency compilation lives in its own
# image layer: as long as the dependency graph is unchanged the `cook` layer is a
# cache HIT and only first-party crates (pollis-delivery + workspace members such
# as pollis-core) recompile on an ordinary code change. cargo-chef is chosen over
# a `RUN --mount=type=cache` on $CARGO_HOME/target because the `cook` result is a
# real image layer that the buildx `type=gha` cache exports and restores across
# CI runs — BuildKit cache MOUNTS are not reliably exported by that backend. The
# layering only pays off under a persistent BuildKit layer cache — see
# .github/workflows/delivery-deploy-*.yml (buildx + type=gha) and docs/deployments.md.

FROM rust:1-bookworm AS chef
# pkg-config/openssl for TLS deps; clang/libclang for libsql-ffi's bindgen;
# cmake/cc for native builds.
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev build-essential cmake clang libclang-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
WORKDIR /src

# Distil the dependency graph into recipe.json. First-party source changes do NOT
# change the recipe, so the expensive `cook` below stays a cache hit across deploys.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
# Compile ONLY the dependencies. This is the cache-friendly layer — invalidated
# only when the dependency graph (recipe.json) changes, not on an ordinary code change.
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release -p pollis-delivery --recipe-path recipe.json
# Now the real source; only first-party crates recompile against the already-built deps.
COPY . .
# Bake the source git SHA into the binary (via `option_env!("GIT_SHA")`, exposed
# at `/version`) so a deploy can verify the new image is actually running. Set
# AFTER `cook` so a changed SHA never busts the cached dependency layer.
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
# no-op. Passed as a buildx `--build-arg BUILD_NONCE` by the deploy workflow.
ARG BUILD_NONCE=0
RUN echo "$BUILD_NONCE" > /etc/pollis-build-nonce
EXPOSE 8788
ENTRYPOINT ["/usr/local/bin/pollis-delivery"]
