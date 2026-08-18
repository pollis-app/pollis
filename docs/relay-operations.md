# Relay operations & the operational-separation commitment (v0 + v1)

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
  tier and Turso and can join their logs. **v1 (#813) is now built** — multi-hop
  onion, peer-hosted middle hops, first-party guard *and* last hop (design §6.2,
  §7.1, §13) — and it makes the per-relay position structural: no *single* relay
  holds both the client's IP and the destination. Read the limit precisely
  before repeating it as an operator claim:
  - **Every hop verifies the client's device cert, so every hop learns which
    account built the circuit.** v1 delivers network-address unlinkability, not
    anonymity. Hiding the account needs anonymous credentials, which is a
    separate effort and is not built (design §13).
  - **A pool of only first-party nodes is still one operator at both ends.** With
    a guard and an exit that we both run, we can still join IP↔destination by
    correlating our own two nodes; what v1 removes is any *one* node holding
    both. The arrangement that genuinely breaks the join is a **peer** middle
    hop in a different trust domain — which is why volunteer relays exist, and
    why a peer is never allowed the guard or exit position.
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

- Each device already carries a **device cert**: a 2420-byte ML-DSA-44 signature
  by the user's long-lived **account identity key** binding the device's MLS
  signing public keys to the account (`account_id_pub`). Since #668 this is a
  **v2** cert (domain `pollis-device-cert-v2\x00`): one signature certifies
  **both** of a device's MLS signing keys — the Ed25519 key and the ML-DSA-44 key
  — so neither was ever uncertified during the classic→PQ overlap. #669 ended the
  overlap by retiring the classic suite; the cert format and both keys are
  unchanged, so a device stays admissible under either signature scheme. It is
  minted by
  `pollis-core::commands::account_identity::sign_device_cert` and published to
  `user_device` at enrollment (`ensure_device_cert`). It is the same primitive
  clients already use to admit each other into MLS groups.
- The primitive itself lives in a standalone, dependency-light crate
  **`pollis-device-cert`** (payload format + `verify_device_cert`, deps =
  `ml-dsa` + std). `pollis-core` mints certs and re-exports the verifier;
  `pollis-relay` depends on the same crate and verifies certs at its handshake.
  One source of truth, frozen by a golden test vector, so the mint and verify
  halves can never drift — and, critically, **`pollis-relay` does not depend on
  `pollis-core`** (which would be a cycle *and* would drag the metadata plane
  into the relay binary).
- At the relay handshake (wire **v3** since #668) the client **presents** both of
  its device signing keys + the cert chain (`account_id_pub`, `device_cert`,
  `identity_version`, `issued_at`). The relay verifies two things, both offline:
  1. **Possession** — the handshake signature checks out under the presented
     ML-DSA-44 device key (skew-bounded, nonce'd);
  2. **Membership** — `verify_device_cert` confirms the account key certified
     those device keys.

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

- **Transparency-log anchoring of `account_id_pub` — built in v1, off by
  default.** Verifying the cert proves *device ∈ account* offline. It does **not**
  prove that `account_id_pub` is the account's *published* key in the account-key
  transparency log. Without that, the relay accepts any self-consistent
  `(account key, device key)` pair, including one an attacker generated
  wholesale — and since per-account rate limits key on `sha256(account_id_pub)`,
  an attacker with an unlimited supply of self-consistent account keys has an
  unlimited supply of distinct rate-limiting identities.
  #813 closes this with the **client-presented inclusion proof**, not a fetch —
  see "Account anchoring (#813 Phase E2)" below. It is enabled per node and off
  until clients ship the frame.
- **No live DEVICE revocation (v1).** A revoked device's cert still verifies until
  the account's `identity_version` rotates (which re-issues sibling certs). There
  is no per-connection revocation check in v0 (that would be a metadata-plane
  query). Documented tradeoff; live device revocation is still a v1 item.
  **Live RELAY revocation is a different thing and is built** — see "Live relay
  revocation (#813)" below.

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
  rather than silently dropping a send. **Peers are NOT relays in v0**; peer-run
  relays landed with v1 (#813) and are a **middle hop only** — never the guard,
  never the exit — so the pool is still what carries every circuit's first and
  last hop. Relaying grants no read access of any kind: a relay only ever
  forwards sealed TLS bytes to a pinned first-party host (design §8).
- **Parked peer relays (#813 v1).** A consenting device cannot accept inbound
  connections, so it **dials out** to each node in the pool, runs the ordinary
  device handshake, and leaves the connection parked (design §7.1). A node
  therefore holds a small in-memory registry of parked peers and, on an `Extend`
  naming a parked peer's leaf fingerprint, splices into that connection instead
  of dialling. Operationally this needs **no configuration** — a v4 node that is
  keyed (above) does it automatically — and it changes nothing about what a node
  stores: the registry is `sha256(peer relay leaf) -> live socket` plus that
  leaf, held in memory only, reaped when the connection ends, and never
  persisted or logged. No client identity, no address, no traffic metadata, no
  counts of who is connected. A peer's leaf is public in the signed directory by
  construction (clients must pin it), which is why the health port may publish
  it; nothing else about the registry may ever follow it out. Revocation applies
  to a parked next hop exactly as it does to a dialled one.

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
  keepalives) — recovery is lazy, the same posture as `RemoteDb::with_retry`
  (posture only; no libsql stream is involved here): the cooldown
  expires and the next connect that reaches the endpoint retries it.
- **Fail-open.** Healthy endpoints are tried first, but if **all** are marked dead
  they are still tried — a transient outage that marked the whole pool dead must
  never make it permanently unusable.
- **Load spread.** A rotating start index deals connects across healthy endpoints
  rather than always hammering endpoint 0.
- **Bounded dial.** Each dial has an upper timeout (8s) so an *unreachable* relay
  (packets dropped, no ICMP) fails over fast instead of hanging on the QUIC
  handshake timeout.

### Live relay revocation (#813)

Distinct from **device** revocation, which is still the v1 item noted above. This
is how a *relay* stops being trusted without waiting for the signed directory's
~1 hour TTL to lapse.

**The shape.** A second signed artifact — `revocations.json` — sits beside the
directory, signed by the same Ed25519 directory key, with a **5-minute** TTL and
re-signed on every 2-minute reconcile. It names revoked relays by advertised
`addr` and/or by `cert_sha256_b64` (base64 of SHA-256 over the DER leaf). The
directory carries an additive `revocation: { seq, path, count }` anchor stating
the minimum sequence a paired list must have.

**Why two artifacts instead of just shortening the directory TTL.** The
directory's long TTL is a deliberate *availability* property: a wedged reconciler
must not take the pool down with it. Freshness is a *safety* property and wants
the opposite. Splitting them lets both be right — the same reason a certificate
and its CRL have different lifetimes. The exposure window for a seized relay is
now the revocation TTL, not the directory TTL.

**Fail-closed, in every direction.** A forged, unsigned, expired or replayed
revocation list never *grants* access; it removes the evidence, and "cannot
evaluate revocation" resolves the same way as "revoked". Concretely:

- No list, an expired list, or a list below the directory's anchored sequence ⇒
  **no usable relays** (in `prefer` that means a direct dial, in `strict` a
  surfaced degrade — never a silent send over a possibly-seized relay).
- A revocation entry keyed on a selector the build does not understand fails the
  **whole list** closed, rather than being skipped — skipping would silently
  downgrade a real revocation into an admission.
- A relay with no directory key configured cannot evaluate revocation, so it
  refuses to **extend** circuits to other relays (single-hop `CONNECT` is
  unaffected — there is no next hop to vouch for). The unconfigured case is the
  strictest state, not the loosest; making it permissive is how revocation
  quietly becomes decoration.
- Sequence numbers come from the revocation parameter's SSM `Version`, so
  rollback protection is server-side and monotonic by construction. Clients keep a
  high-water mark and reject anything below it.

**Enforcement points.** `pollis_relay::policy::RevocationStore` is the shared,
pure decision point (`admit` → `Admit | Revoked | Unevaluable`, and only `Admit`
is an admission); the reconciler is the producer and additionally drops revoked
relays from the directory and terminates the matching nodes. Runbook:
`infra/relay-hydra/README.md` → "Revoke a relay NOW".

On a **relay node** the store gates `Extend` in `server.rs::serve_extend`, twice:

1. **before the dial**, on the next hop's advertised address — so a seized node is
   never even contacted;
2. **after the QUIC handshake**, on the full identity including the DER leaf the
   peer actually presented — which is what binds the `cert_sha256_b64` selector,
   the one that matters once nodes stop sharing a pooled identity (§7 / peer-hosted
   relays).

Both refuse with the existing typed `ExtendFailed`. **Operator consequence: a node
with no `directory_key_b64` configured cannot be a middle hop at all** — it cannot
evaluate revocation, and "cannot evaluate" resolves the same way as "revoked". It
still serves circuits that terminate on it, so single-hop is unaffected. A node
keeps its list current by fetching `revocation_url` every ~60s with backoff
(`pollis_relay::revocation_sync`); a wedged fetch degrades the node to
"no longer a middle hop", never to "still trusting a stale list", because expiry
is checked when `admit` is called rather than when a list is installed.

### Account anchoring (#813 Phase E2)

The relay can require a client to **prove its account key is published in the
account-key transparency log**, and does it with **zero I/O** — the client
presents the leaf, an RFC-6962 inclusion proof, and the ML-DSA-44 signed tree
head, all in one optional v4 `Anchor` frame; the relay checks them against one
pinned log key. A relay-side *fetch* was the alternative and is ruled out: it
would put the relay back inside the metadata plane, which is the coupling §2
exists to remove.

The relay verifies, cheapest check first:

1. the leaf's `(user_id, identity_version, account_id_pub)` are **exactly** the
   handshake's — anchors are public data, so an unbound proof proves nothing;
2. the head is inside `anchor_max_age_secs`;
3. the head verifies under the pinned key **in the account-keys domain context**
   (one key signs three trees; only the context separates them);
4. the audit path carries the leaf into that head's root.

Three enforcement rungs, set by config:

| `transparency_key_hex` | `require_account_anchor` | Behaviour |
|---|---|---|
| unset | false | No anchoring claim. An anchor is neither required nor judged. **Default.** |
| set | false | Every presented anchor is verified; a bad one is `Unauthorized`. Absence is served. |
| set | true | As above, plus a client presenting none is refused with `AnchorRequired`. |

Setting `require_account_anchor` without the key **aborts startup** rather than
coming up unenforcing.

**Roll it out publish-then-enforce.** Requiring anchors before clients ship the
frame locks the fleet out; so does requiring them for an account that has not yet
reached the log's **daily** publish (a signup or key rotation is invisible for up
to ~24h — the same window that produces `AuditStatus::Pending` in the app). Ship
the mechanism, get clients presenting anchors, then flip the flag.

**What it does not prove.** That the presented `identity_version` is the account's
*current* one — that needs the whole tree, i.e. a fetch. A rotated-away key keeps
verifying, exactly as its device certs do; this is the same "no live DEVICE
revocation" limit below, not a new one. `anchor_max_age_secs` bounds how stale a
*head* may be and cannot bound key rotation.

**Backward compatibility.** The §3 directory payload stays at `version: 1` and
every existing field keeps its exact meaning; the anchor is one additive optional
top-level field that already-shipped clients ignore. Those clients do not enforce
revocation — nothing can make a shipped binary do that — but they are not left
where they were: a revoked node leaves `relays[]` within one reconcile, and the
directory TTL is cut to 5 minutes while any revocation is active, so their
exposure drops from up to an hour to minutes with no client change.

## 5. Turnkey deploy runbook

The steps below are the **manual VPS/host** roll. For a **hands-off AWS pool** that
provisions the nodes, hosts a signed relay directory, and self-heals/scales with no
human in the loop, use **the hydra** (`infra/relay-hydra/`, #616) instead — it wraps
the same image + config in Terraform + a reconciler Lambda, and it draws each node's
AWS region at random from the allowed US set on a rotation interval — the pool moves
without any client change, because the client pins the shared identity cert and not
an address (see "Relay pool & failover" above). The manual steps here remain the
reference for the config shape and the per-node contract.

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

# Act as a MIDDLE HOP of a multi-hop circuit — honour `Extend` by dialing the
# next relay (wire v4, design §6.2). Default true. Set false to make this node
# last-hop-only; it still serves circuits that terminate here, and the
# destination allowlist above is enforced exactly the same either way (a middle
# hop never sees a destination at all, so the allowlist simply never applies to
# forwarded traffic).
allow_extend = true

# Base64 (STANDARD) of the 32-byte Ed25519 key that signs the relay directory AND
# the revocation list — the same POLLIS_OVERLAY_DIRECTORY_KEY clients pin.
# REQUIRED to actually be a middle hop: without it this node cannot evaluate
# revocation, and "cannot evaluate" resolves the same way as "revoked", so every
# `Extend` is refused regardless of `allow_extend` above. Omitting it is safe,
# never permissive.
directory_key_b64 = "<terraform output relay_directory_public_key>"
revocation_url = "https://relays.pollis.com/revocations.json"

# Transparency-log account anchoring (#813 Phase E2). Omitted ⇒ this node makes
# no anchoring claim. Do NOT set require_account_anchor until clients ship the
# `Anchor` frame — see "Account anchoring" above.
# transparency_key_hex = "<the account-key tree's ML-DSA-44 public key, hex>"
# require_account_anchor = false
# anchor_max_age_secs = 604800

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

> **On a MULTI-node pool, pin an immutable image — not `:latest`.** A mutable tag
> never updates a running node (Docker caches by content hash and
> `--restart=always` never re-pulls), so a rolling `:latest` push leaves the pool
> split across two relay generations; because the relay bumps its ALPN with
> `PROTOCOL_VERSION`, a client that reaches a wrong-generation node just fails QUIC
> ALPN. Pull `ghcr.io/actuallydan/pollis-relay@sha256:<digest>` (or the immutable
> `:<git-sha>` tag), roll one node at a time, and confirm each `GET /version`
> reports the intended `sha` **and** `protocol` before moving on. The automated AWS
> pool does this for you — see `infra/relay-hydra/README.md` → "Roll the relay
> image" (#703).

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

> **The directory-signing key stays Ed25519 — deliberately out of scope for the
> post-quantum authentication migration (#668).** #668 moved every *identity*
> signature (account key, device cert, relay handshake, DS request auth) to
> ML-DSA-44, but the directory envelope is minted by a Node Lambda and
> `node:crypto` has no ML-DSA implementation, so there is no way to sign it
> post-quantum without pulling a third-party PQ signer into the reconciler's
> zero-dependency minting path. The exposure is also different in kind: a
> directory signature is only useful *live* — the envelope names which
> first-party relays to dial right now, carries a short TTL (default one hour)
> and is re-signed on every reconcile, and each entry is still pinned to its own
> QUIC cert. Nothing about a captured envelope becomes forgeable-in-hindsight,
> so there is no harvest-now/forge-later exposure of the kind that motivated
> #668. Revisit once a vetted ML-DSA signer exists for the minting tier.

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

- **Image is live:** `curl http://<host>:9445/version` →
  `{"service":"pollis-relay","sha":"<GIT_SHA>","protocol":"pollis-relay/4","protocol_version":4}`.
  (`protocol` reports the **newest** generation the node speaks; a v4 node also
  still accepts `pollis-relay/3` clients, so an upgraded node keeps serving the
  shipped single-hop fleet. The reconciler's expected-protocol setting must be
  moved to `pollis-relay/4` in the same roll, or it will exclude every upgraded
  node from the signed directory.)
  `sha` is the build `relay-image.yml` produced (mirrors the DS `/version`
  tripwire — don't trust "container started", confirm the running build);
  `protocol` is the relay's ALPN/wire identity, which the hydra reconciler uses to
  keep a wrong-generation node out of the signed directory and to cycle stale
  builds (#703). `curl http://<host>:9445/health` → `ok`.
- **Middle-hop readiness — `/version` and `/health` do NOT tell you this, so check
  it separately.** A v4 node rolled **without** `directory_key_b64` +
  `revocation_url` comes up perfectly **healthy**, reports `pollis-relay/4`, is
  admitted to the signed directory, and serves single-hop `CONNECT` normally —
  while **silently refusing every `Extend`**. It cannot evaluate revocation, and
  "cannot evaluate" resolves the same way as "revoked" (see "Live relay
  revocation" above), so multi-hop just quietly does not work and nothing in the
  probes says so. Confirm both keys are actually in the running node's config
  before calling a roll done, and confirm the reconciler is publishing
  `revocations.json`; a client that reaches the node sees only an `ExtendFailed`,
  which is indistinguishable from an unreachable next hop.
- **Traffic actually routes:** a client with the overlay in **Prefer** mode (and a
  reachable pool) sends its control-plane traffic (Turso reads, DS writes) through
  a relay — the node's logs show authorized handshakes + dials to the allowlisted
  hosts, and the client's source IP no longer appears at Turso/DS.
- **Parked peers (only once volunteers exist):** `curl http://<host>:9445/peers` →
  `{"peers":[{"cert_b64":"..."}]}`, or `{"peers":[]}` when nobody is parked. This
  is what the directory reconciler aggregates into the directory's additive
  `peers` field; an empty list is the normal state for a fresh pool and is not a
  fault.

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
