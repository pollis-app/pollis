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

## 5. Residual leaks stated honestly (design §14.4)

v0 does not paper over what still leaks: the `ureq` transparency-**verify** path
can't proxy (public-key fetching, not account-keyed activity); Expo push
registration is a non-first-party host and stays direct; LiveKit signaling stays
direct by the plane split. See design §14.4 for the full list and the planned
dispositions.
