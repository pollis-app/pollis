# Pollis closed-overlay relay image (design §7 first-party pool node).
#
# Build from the REPO ROOT (the whole cargo workspace must resolve):
#   docker build -f pollis-relay/Dockerfile -t pollis-relay .
#   docker build -f pollis-relay/Dockerfile --build-arg GIT_SHA=$(git rev-parse HEAD) -t pollis-relay .
#
# Run (VPS/host model — the relay owns a public UDP port; there is no reverse
# proxy in front, it terminates its OWN outer QUIC):
#   docker run -d --name pollis-relay \
#     -p 9444:9444/udp -p 9445:9445 \
#     -v pollis-relay-data:/var/lib/pollis-relay \
#     -e POLLIS_RELAY_CONFIG=/etc/pollis-relay/relay.toml \
#     -v /etc/pollis-relay:/etc/pollis-relay:ro \
#     pollis-relay
#
# Required config (file, env, or CLI — see pollis_relay::config): the UDP `bind`,
# the destination `allowlist` (the four first-party hosts), the persisted
# `identity_path` (mount a volume so the pinned QUIC identity survives restarts —
# generated on first start), the `health_bind`, and rate limits. See
# docs/relay-operations.md for the full runbook.
#
# Ports: 9444/udp (QUIC relay, default) + 9445/tcp (health, if `health_bind` set).
#
# The relay does raw DNS + TCP dial to allowlisted hosts and forwards the client's
# OWN sealed TLS bytes — it never terminates the inner TLS, so it needs NO
# ca-certificates in the runtime (it verifies no server cert of its own). Only
# pollis-relay + its deps compile here — none of the desktop/media/Turso crates are
# in its dependency closure, so no media/libsql system libraries are needed.

FROM rust:1-bookworm AS build
# build-essential/cmake/cc for native builds (ring's assembly); no openssl/clang —
# the relay uses rustls (ring) and pulls in no libsql/bindgen.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Bake the source git SHA into the binary (via `option_env!("GIT_SHA")`, exposed
# at `/version`) so a deploy can verify the new image is actually running.
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}
RUN cargo build --release -p pollis-relay \
    && strip target/release/pollis-relay

FROM debian:bookworm-slim AS runtime
COPY --from=build /src/target/release/pollis-relay /usr/local/bin/pollis-relay
# UDP relay port (QUIC) + TCP health port. Documentation only — `docker run -p`
# still has to publish them.
EXPOSE 9444/udp
EXPOSE 9445/tcp
ENTRYPOINT ["/usr/local/bin/pollis-relay"]
