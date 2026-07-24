# Relay operations & the operational-separation commitment (v0)

*Companion to `docs/relay-overlay-design.md` (esp. §11.1, §12, §14). Required
before the v0 overlay ships, per the §11.1 open question and issue #455. This
doc states plainly what the v0 relay tier is, what it defends, and — crucially —
the concrete mechanism that keeps it **operationally separate from the Turso
metadata plane**.*

## 1. What v0 honestly is (and is not)

v0 is the **single-hop, first-party-operated fallback relay pool** (design §6.1,
§12). Its honest, bounded value:

- **Defends:** network-layer IP metadata. A relay hides the client's source IP
  from Turso / the DS / R2, so those services — and anyone who breaches them or
  subpoenas their logs — see the relay's IP, not the user's household/network
  (design §2.1). This is **breach / subpoena defense + IP-unlinking from the
  metadata plane.**
- **Does NOT defend:** a *malicious* first-party operator who runs both the relay
  tier and Turso and can join their logs. Removing that trust assumption is
  **v1** (multi-hop onion, peer-hosted relays, first-party last hop — design
  §6.2, §14.0). The roadmap north star is explicitly Option B; v0 is the
  waypoint that ships the plumbing and the honest win in the meantime.
- **Does NOT defend:** the application-layer social graph (that is sealed sender,
  a separate project — design §2.2), the global passive adversary (§2.3), or
  traffic-fingerprinting by cadence (§11.2). And the relay is **never** part of
  or proof of the E2EE guarantee — that proof is verifiable builds + the
  transparency log (design §3, §12).

This is the **B-direction choice**: "trust us, we operationally separate the
tiers" is unverifiable and contrary to Pollis's verify-don't-trust ethos, so v0
does not oversell it — it ships the breach/subpoena win and moves the
malicious-operator defense to v1 where it is *structural*, not a promise.

## 2. The concrete separation mechanism: offline device-cert auth

The single most important property v0 must provide is that **the relay tier
holds no Turso credentials and makes no metadata-plane query.** If the relay had
to look a device up in Turso to authenticate it, the relay would *be* part of the
metadata plane — it would see (and could log) which account connected from which
IP, defeating the whole point.

v0 achieves the separation by authenticating connecting devices with an **offline
device-certificate chain** — verified locally, with **zero I/O**, no Turso query,
no network call per connection:

- Each device already carries a **device cert**: a 64-byte Ed25519 signature by
  the user's long-lived **account identity key** binding the device's MLS signing
  public key to the account (`account_id_pub`). This is minted by
  `pollis-core::commands::account_identity::sign_device_cert` and published to
  `user_device` at enrollment (`ensure_device_cert`). It is the same primitive
  clients already use to admit each other into MLS groups.
- The primitive itself lives in a standalone, dependency-light crate
  **`pollis-device-cert`** (payload format + `verify_device_cert`, deps =
  `ed25519-dalek` + std). `pollis-core` mints certs and re-exports the verifier;
  `pollis-relay` depends on the same crate and verifies certs at its handshake.
  One source of truth, frozen by a golden test vector, so the mint and verify
  halves can never drift — and, critically, **`pollis-relay` does not depend on
  `pollis-core`** (which would be a cycle *and* would drag the metadata plane
  into the relay binary).
- At the relay handshake the client **presents** its device signing key + the
  cert chain (`account_id_pub`, `device_cert`, `identity_version`, `issued_at`).
  The relay verifies two things, both offline:
  1. **Possession** — the handshake signature checks out under the presented
     device key (skew-bounded, nonce'd);
  2. **Membership** — `verify_device_cert` confirms the account key certified
     that device key.

  Together: "a cryptographically self-consistent Pollis device." No lookup, no
  DB, no secret on the relay.

Because that check needs nothing from Turso, a relay node can run with **no Turso
URL, no Turso token, no DS credentials** — it is genuinely outside the metadata
plane. That is the mechanism, not a policy promise.

### Why "well-formed device, rate-limited" is enough anti-abuse for v0

The relay's destination allowlist is closed to first-party hosts only (design
§1.2) — there is **no open-proxy abuse surface**: a client can only reach Turso /
DS / R2 / the relay's own pinned hosts, never an arbitrary third party. So the
gate the relay needs is just "is this a real Pollis device?" plus rate limiting
(below) to blunt a single captured/misbehaving device. Full anti-Sybil and
abuse-at-scale defenses are a v1 concern that rides on multi-hop path selection.

### Honest limits of the offline chain (v0 → v1)

- **No transparency-log anchoring of `account_id_pub` (v1).** Verifying the cert
  proves *device ∈ account* offline. It does **not** prove that `account_id_pub`
  is the account's *published* key in the account-key transparency log — doing so
  requires either a fetch or a client-presented inclusion proof. v0 deliberately
  does **not** build a transparency-log fetch into the relay, because that would
  re-introduce the network coupling this whole design removes. Consequence: the
  relay accepts any self-consistent `(account key, device key)` pair, including
  one an attacker generated wholesale — which is fine for v0, whose gate is
  "well-formed device + closed allowlist + rate limit," not "this specific
  account." Log-anchoring the account id is a documented **v1** hardening.
- **No live revocation (v1).** A revoked device's cert still verifies until the
  account's `identity_version` rotates (which re-issues sibling certs). There is
  no per-connection revocation check in v0 (that would be a metadata-plane
  query). Documented tradeoff; live revocation is a v1 item.

## 3. Rate limiting & abuse control

The relay applies in-memory token-bucket + concurrency limits, keyed on **both**
the source IP and the authenticated `account_id_pub` (design §11.5):

- new-circuit rate (per-minute token bucket) per IP and per account;
- max concurrent circuits per IP and per account;
- a global concurrent-connection cap.

On breach the relay returns a clean `Rejected(RateLimited)` rather than dropping
the stream. All limits are tunable from the config file; defaults are generous
(the closed allowlist already removes open-proxy abuse). No external store — the
counters are process-local, which keeps rate limiting itself off the metadata
plane.

## 4. Deploy shape

- **Stateless, disposable, rotatable nodes.** A relay node holds only its own
  QUIC identity keypair (self-signed; clients pin the leaf cert). Generate it on
  first start and persist it (`--identity <path>`; cert at `<path>.crt`) so
  restarts keep the same pinned identity — or delete the files to rotate to a
  fresh identity. No user data, no metadata, ever persisted.
- **Config file (TOML).** `--config <path>` / `POLLIS_RELAY_CONFIG` sets bind
  address, destination allowlist, identity path, rate-limit params, and the
  concurrent-connection cap; CLI flags and env vars override the file (see
  `pollis_relay::config` module docs for the format). There is **no devices
  file** — trust flows from the cert the client presents, not an operator table.
- **Graceful shutdown.** On SIGTERM/SIGINT the node stops accepting new
  connections, lets in-flight pipes drain for a bounded deadline, then exits 0 —
  so a rolling redeploy doesn't sever active sessions.
- **First-party pool guarantees messages-must-work.** The pool exists so the
  network functions even with zero volunteer peers (design §7, §10.3); overlay
  use is opt-in (`off → prefer → strict`), and `strict` surfaces a degraded state
  rather than silently dropping a send. **Peers are NOT relays in v0** — peer-run
  relays are a v1 feature. Relaying grants no read access of any kind: a relay
  only ever forwards sealed TLS bytes to a pinned first-party host (design §8).

### Relay pool & failover (client-side)

The client can be pointed at **multiple first-party relay endpoints**
(`POLLIS_OVERLAY_RELAY` is a comma-separated list, e.g.
`relay1.pollis.com:443,relay2.pollis.com:443`). This is what makes
"messages must work" real when the overlay is on: a single dead relay never
wedges delivery. The pool is entirely single-hop — it decides *which* first-party
relay to dial, not *how many hops* (multi-hop/onion is v1). One shared pinned cert
(`POLLIS_OVERLAY_RELAY_CERT`) covers every endpoint in v0 (all first-party, same
identity).

`RealRelayFactory` (`pollis-core/src/net/overlay.rs`) implements it:

- **Failover.** Each `connect` tries the endpoints in health order and returns
  the FIRST success. Every attempt is `resolve addr → build_single_hop → connect`;
  on failure the endpoint is marked dead and the next candidate is tried. Only
  when **every** candidate fails does `connect` error — so `prefer` still falls
  back to direct and `strict` still degrades (surfaced), but only after the whole
  pool is exhausted, never on one dead relay.
- **Mark-dead-on-failure + cooldown (no background poll).** Health is tracked
  inline: a failed dial marks that endpoint dead for a cooldown (30s), a success
  clears it. There is deliberately **no health-poll loop** (repo rule: no periodic
  keepalives) — recovery is lazy, matching `RemoteDb::with_retry`: the cooldown
  expires and the next connect that reaches the endpoint retries it.
- **Fail-open.** Healthy endpoints are tried first, but if **all** are marked dead
  they are still tried — a transient outage that marked the whole pool dead must
  never make it permanently unusable.
- **Load spread.** A rotating start index deals connects across healthy endpoints
  rather than always hammering endpoint 0.
- **Bounded dial.** Each dial has an upper timeout (8s) so an *unreachable* relay
  (packets dropped, no ICMP) fails over fast instead of hanging on the QUIC
  handshake timeout.

## 5. Turnkey deploy runbook

The steps below are the **manual VPS/host** roll. For a **hands-off AWS pool** that
provisions the nodes, hosts a signed relay directory, and self-heals/scales with no
human in the loop, use **the hydra** (`infra/relay-hydra/`, #616) instead — it wraps
the same image + config in Terraform + a reconciler Lambda. The manual steps here
remain the reference for the config shape and the per-node contract.

Everything below is turnkey from this repo **except provisioning the hosts and
DNS** — spinning up the VMs and pointing names at them is the operator's ops (the
one thing the codebase can't do for you). The relay deploys on the **VPS/host
model** (a public UDP/QUIC port, like LiveKit — **not** a Cloudflare Worker,
because the overlay hop is QUIC). The image is built + published by
`.github/workflows/relay-image.yml` to `ghcr.io/actuallydan/pollis-relay`; there
is **no auto-deploy**, so the roll is manual per the steps here.

Reaffirming the **§11.1 posture** the whole tier rests on: v0 is
**breach/subpoena defense + IP-unlinking** from the metadata plane. A relay node
holds **no Turso URL/token and no DS credentials** — it authenticates devices with
the offline device-cert chain and makes zero metadata-plane queries. Keep it that
way: never hand a relay node a Turso secret.

### Step 1 — mint the pinned relay QUIC identity (once)

The relay's identity **is** its self-signed QUIC leaf cert; clients pin that exact
cert rather than trusting a CA. Generate it once and reuse it across the whole
pool (v0: all endpoints are first-party and share one pinned identity):

```bash
# First start writes identity.key + identity.key.crt (raw DER) if absent.
pollis-relay --identity /var/lib/pollis-relay/identity.key --bind 0.0.0.0:9444 --allow example.invalid
# Grab the DER cert the clients must pin (base64 for baking into the client build):
base64 -w0 /var/lib/pollis-relay/identity.key.crt   # → POLLIS_OVERLAY_RELAY_CERT
```

- **How the pinned cert reaches the client:** it is baked into the client build as
  `POLLIS_OVERLAY_RELAY_CERT` (base64 of the DER, or a filesystem path) — Step 3.
  There is no cert-fetch on the trust path: the client trusts exactly the bytes
  compiled in, so a swapped relay cert can't be silently accepted.
- **Sharing vs per-node:** minting **one** identity and copying `identity.key` +
  `identity.key.crt` to every node is the v0 default (one `POLLIS_OVERLAY_RELAY_CERT`
  covers the pool). Alternatively mint one per node and bake a cert **list** — but
  v0's client pins a single shared cert, so share one identity. Rotate by deleting
  both files (a fresh identity is minted on next start) and re-baking the client.

### Step 2 — run N nodes across ≥2 unrelated providers

Provision (operator ops) ≥2 hosts on **unrelated** providers/networks (so one
provider's outage or subpoena doesn't take the whole pool). On each, run the
published image with a config file:

```toml
# /etc/pollis-relay/relay.toml
bind = "0.0.0.0:9444"
# The FOUR first-party destinations, resolved from THIS deployment's real
# hostnames (prod shown; a dev pool uses api-dev.pollis.com etc.):
#   Turso (metadata reads), the DS (writes), R2 (media/CDN), LiveKit (media SFU).
allowlist = [
  "*.turso.io",
  "api.pollis.com",
  "cdn.pollis.com",
  "livekit.pollis.com",
]
identity_path = "/var/lib/pollis-relay/identity.key"
health_bind = "0.0.0.0:9445"

[rate_limit]
new_circuits_per_min_per_ip = 600
new_circuits_per_min_per_account = 600
max_concurrent_per_ip = 256
max_concurrent_per_account = 128
```

```bash
docker run -d --name pollis-relay \
  -p 9444:9444/udp -p 9445:9445 \
  -v pollis-relay-data:/var/lib/pollis-relay \
  -v /etc/pollis-relay:/etc/pollis-relay:ro \
  -e POLLIS_RELAY_CONFIG=/etc/pollis-relay/relay.toml \
  ghcr.io/actuallydan/pollis-relay:latest
```

Open the host firewall for **`9444/udp`** (the QUIC relay) and the **health TCP
port** (`9445`, scoped to your orchestrator/LB rather than the public internet if
you can). Mount the identity volume so a restart keeps the same pinned cert.

### Step 3 — point clients at the pool

Bake the pool into the client build (the same `option_env!` mechanism as every
other first-party endpoint; a runtime env var of the same name overrides for local
testing). There are two ways to supply the pool — pick ONE:

**A. Dynamic signed directory (recommended — the hydra, #616).** The client fetches
a signed, self-updating directory of live relays and refreshes it as pool
membership changes (Spot reclamation / self-heal node replacement), so you never
re-ship the client when nodes rotate:

- `POLLIS_OVERLAY_DIRECTORY_URL` = the stable directory URL, e.g.
  `https://relays.pollis.com/directory.json`.
- `POLLIS_OVERLAY_DIRECTORY_KEY` = the base64 (STANDARD) of the 32-byte Ed25519
  directory-signing **public** key (printed by `infra/relay-hydra/scripts/mint-signing-key.sh`).

The client (`crate::net::directory` → `SwappableFactory` in `net/overlay.rs`)
fetches the envelope, verifies the Ed25519 signature over the exact payload bytes
against the pinned key, and rejects (fail-closed → `prefer` direct / `strict`
degrade) on bad signature, `version != 1`, expiry, malformed JSON, or empty relays
— the §3 contract, byte-identical to `infra/relay-hydra/lib/directory-verify.mjs`.
It refreshes near the directory's `expires_at` (and on-demand when the pool is
exhausted), swapping the live pool with **no shim restart**. Each directory entry
carries its own pinned cert, so per-node identities work automatically. When BOTH
vars are set, this path supersedes `POLLIS_OVERLAY_RELAY` below.

**B. Static endpoint list (v0 / operator-provisioned hosts).** For a hand-managed
pool with stable addresses:

- `POLLIS_OVERLAY_RELAY` = the **comma-separated** endpoint list, e.g.
  `relay1.pollis.com:9444,relay2.pollis.com:9444`.
- `POLLIS_OVERLAY_RELAY_CERT` = the pinned cert from Step 1 (base64 of the DER, or
  a path to the DER file).

Either way, the client (`RealRelayFactory`, `pollis-core/src/net/overlay.rs`) treats
the resolved set as a **pool** (design §4 above): it dials endpoints in health order
and takes the first success; a failed dial marks that endpoint dead for a 30 s
cooldown and the next candidate is tried; only when **every** endpoint fails does a
connect error (so `prefer` still falls back to direct, `strict` still degrades — but
never on a single dead relay). A rotating start index spreads load; each dial is
bounded (8 s) so an unreachable node fails over fast. In the static path one shared
`POLLIS_OVERLAY_RELAY_CERT` pins every endpoint; the directory path pins each node
against its own advertised cert.

### Step 4 — verify

- **Image is live:** `curl http://<host>:9445/version` → the SHA `relay-image.yml`
  built (mirrors the DS `/version` tripwire — don't trust "container started",
  confirm the running build). `curl http://<host>:9445/health` → `ok`.
- **Traffic actually routes:** a client with the overlay in **Prefer** mode (and a
  reachable pool) sends its control-plane traffic (Turso reads, DS writes) through
  a relay — the node's logs show authorized handshakes + dials to the allowlisted
  hosts, and the client's source IP no longer appears at Turso/DS.

### The ops boundary, stated plainly

**Turnkey here:** the image, the health/version probe, the CI build+publish, the
config shape, and every step above. **Operator ops (not automatable from this
repo):** provisioning the VMs, opening the firewall, and the DNS records that make
`relay1.pollis.com` etc. resolve. Once a host + name exist, the roll is `docker
run` + a client rebuild with the two env vars.

## 6. Residual leaks stated honestly (design §14.4)

v0 does not paper over what still leaks. The `ureq` transparency-**verify** path
is **no longer** on that list: it now routes through the shim when the overlay is
on (`build_agent` takes an optional SOCKS5 proxy; `pollis-core` passes
`socks5://<shim>` to the `verify_*_via` entry points — `verify.pollis.com` is a
first-party host), so it is closed. What remains: Expo push registration is a
non-first-party host and stays direct; LiveKit signaling stays direct by the
plane split. See design §14.4 for the full list and the planned dispositions.
