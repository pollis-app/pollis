# Design Spike: Opt-In Peer-Relay / Onion Network-Privacy Overlay

**Status:** Design spike — decision document. No code. The team reads this to decide *whether* to build a network-privacy overlay for Pollis, and if so, in what order.
**Audience:** Pollis engineering + whoever owns the security roadmap.
**Scope:** the network / IP-metadata layer only. This is explicitly *not* a redesign of the E2EE protocol, the transparency logs, or the application-layer metadata model. It composes with those; it does not replace them.
**Author's stance:** written to be argued with. Every claim that could be an overclaim is flagged as such. If you skim one section, skim §1 (the thesis) and §12 (the recommendation).

---

## 0. TL;DR

Pollis clients talk to a **fixed, small set of first-party endpoints** — Turso (libSQL), Cloudflare R2, LiveKit, and the `pollis-delivery` Delivery Service — enumerated in `pollis-core/src/config.rs:4-25`. They never talk to arbitrary hosts. That single fact changes everything about what a privacy overlay for Pollis has to be:

- It is a **closed overlay**, not Tor. It only ever forwards to those four first-party services. There is **no exit-node problem** — no relay ever emits traffic to the open internet on a stranger's behalf — which removes the entire abuse/liability class that makes running a Tor exit node legally hazardous.
- It defends **network-layer IP metadata** ("who is connecting from where") against the service operators. It does **not** defend the **application-layer social graph** (sender id, group membership, timestamps), which lives *inside* Turso's data model by design (`docs/security-whitepaper.md` §1.2). Those are two different problems; the relay and a future sealed-sender layer **compose** but neither substitutes for the other.
- It is **not** the proof that Pollis is E2EE. That proof is verifiable/reproducible builds + the transparency log (§6.9 of the whitepaper, `verify.pollis.com`). The relay is metadata **defense-in-depth**. Marketing it as the E2EE guarantee would be the same category of overclaim Pollis (rightly) accuses Meta of.

**Recommendation (full version in §12):** Build it, opt-in, phased. Ship **v0 = single-hop, first-party-operated fallback relay pool** first — it is the smallest slice that delivers real value (IP unlinkability from Turso/DS for users who opt in) and is mostly a transport-plumbing exercise, not a cryptography project. Treat multi-hop (v1) and timing defenses (v2) as later phases gated on real demand and a threat model that justifies the latency cost. De-risk first by prototyping the **local SOCKS-style shim + endpoint re-resolution** in `pollis-core`, because that plumbing is the load-bearing, protocol-agnostic part and is where the surprises will be.

---

## 1. Thesis: A Closed Overlay, Not Tor

### 1.1 The endpoint set is fixed and small

A Pollis client is configured, at build time, with exactly these network destinations (`pollis-core/src/config.rs:4-25`, `Config::from_env` at `:28-70`):

| Config field | Service | Protocol | Role |
|---|---|---|---|
| `turso_url` / `turso_token` | **Turso (libSQL)** | Hrana over HTTP/2, TLS, `libsql://` | Canonical metadata + ciphertext envelope store. Direct connection, no middle server (`ARCHITECTURE.md` §Network architecture). |
| `log_db_url` / `log_db_token` (optional) | **Turso (commit-log DB)** | same | Read-only view of the MLS control-plane tables once split out (`config.rs:7-11`). Same operator, same protocol — not a distinct trust domain. |
| `r2_endpoint` + `r2_*` creds | **Cloudflare R2** | HTTPS + AWS SigV4 | Encrypted attachment / avatar object storage. |
| `livekit_url` / `livekit_api_*` | **LiveKit** | WebSocket signalling + WebRTC (DTLS-SRTP) | SFU for realtime events + voice frames. |
| `pollis_delivery_url` (optional) | **`pollis-delivery` Delivery Service** | HTTPS (axum, `pollis-delivery/src/lib.rs`) | Sole writer to the MLS commit log; also the secrets broker (LiveKit token mint + R2 presign, `docs/secrets-broker.md`). |

That is the **entire** set. There is no user-supplied URL, no federation, no "connect to your friend's server," no link-preview fetcher reaching arbitrary origins, no third-party CDN in the data plane. Note the trajectory here is *toward* consolidation, not away from it: the secrets-broker work (#393/#437) is actively **removing** the last few cases where the client held long-lived secrets and reaching those services *through* the Delivery Service instead (`docs/secrets-broker.md`, "The problem"). The endpoint set is getting smaller and more first-party over time, not larger.

### 1.2 Why this is categorically easier and safer than onion routing

General-purpose onion routing (Tor) has to solve a hard, open-ended problem: forward *arbitrary* user traffic to *arbitrary* destinations on the public internet. That generality is the source of Tor's worst operational problem — the **exit node**. An exit node emits a stranger's traffic (spam, abuse, CSAM crawling, credential stuffing, DMCA-triggering downloads) onto the open internet under the exit operator's IP. Exit operators field abuse complaints, law-enforcement contact, and civil liability, and must run exit policies, abuse-response, and reputation management. This is the single biggest deterrent to running relay infrastructure and the reason exit capacity is chronically scarce.

**Pollis has no exit-node problem, because Pollis has no exits.** A Pollis relay only ever forwards to the four first-party hosts in §1.1. Concretely:

- A relay can enforce a **static allowlist** of destination hosts (the Turso host, the R2 endpoint, the LiveKit host, the DS host). It refuses to forward anywhere else. There is no configuration under which a relay carries traffic to `random-site.example`.
- Therefore a relay operator **can never be the origin IP for abusive traffic aimed at a third party.** The only thing they ever "send" is a connection to Pollis's own infrastructure. The abuse/liability calculus that dooms Tor exits simply does not arise.
- The destination set is **known in advance**, so the overlay can be a **circuit to a known host**, not a general packet router. This dramatically shrinks the design: no exit policies, no DNS-leak surface (destinations are pinned), no need to reason about arbitrary application protocols — the relay forwards opaque TLS/QUIC bytes to one of four pinned TLS endpoints.

This is the whole argument for building something *bespoke and small* rather than embedding Tor: the closed destination set removes ~80% of what makes anonymity networks hard and dangerous to operate. What remains is the genuinely useful core — **breaking the link between a user's IP and a first-party service** — without inheriting the exit-node liability.

**Honest caveat.** "No exit problem" is not "no abuse problem." A relay still consumes a peer's bandwidth and could be used to amplify traffic *toward Pollis's own services* (a relay-assisted DoS on Turso/DS). That is a first-party capacity/rate-limiting problem the operator already has to solve for direct clients; it does not create third-party liability. See §7 (Sybil) and §11 (risks).

---

## 2. What It Defends, and What It Explicitly Does Not

This is the section to be most honest in. The overlay's value is real but narrow, and overclaiming it is the failure mode to avoid.

### 2.1 What it defends: network-layer IP metadata

Today, when a client opens its direct libSQL connection to Turso (`ARCHITECTURE.md`: "1 network hop — simple and fast"), Turso's operator sees the client's **source IP** on every Hrana stream. The whitepaper lists exactly this under what Turso can observe: *"connection patterns (IP address, libSQL Hrana streams)"* (`docs/security-whitepaper.md` §1.2). Same for LiveKit (RTP routing metadata + source IP) and the DS.

With the overlay on, a relayed request reaches the service **wearing the relay peer's IP**. The service operator sees a connection from the relay, not from the originating user. The property delivered is:

> **Unlinkability of a user's real IP to their activity, against the first-party service operator.**

That is worth having. IP is a strong deidentifier: it pins a user to a network, often a household, often a rough geographic location, and (via ISP records) potentially an identity. Removing it from Turso's/LiveKit's view meaningfully raises the bar for an honest-but-curious operator, a subpoena to the operator, or a database compromise that includes connection logs.

### 2.2 What it does NOT defend: the application-layer social graph

This is the critical honesty point. **Hiding the IP does not hide who you are inside Turso's data model.** Pollis's metadata lives in *rows*, not in *packet headers*:

- `users` (id, email, username), `group_member`, `dm_channel_member`, `user_block` — the social graph.
- `message_envelope` — sender id, timestamp, ciphertext size (`docs/security-whitepaper.md` §1.2, `ARCHITECTURE.md` §Data storage model).
- `mls_commit_log` / `mls_welcome` timing.

A request to Turso is **authenticated as a specific user at the application layer** — the actor's `user_id` is supplied and trusted because it came from the unlocked `account_id_key` (`docs/security-whitepaper.md` §8.1). So even if that request arrives from a relay's IP, the *content* of the request says "user `u_123` is reading channel `c_abc`." The relay changes *from where* the connection appears to originate; it does nothing about *whose account* the request is for. Turso still sees the full social graph and message metadata keyed by stable user ids.

> **The relay defends the packet-header identity. It does not defend the payload identity.** Hiding "which household connected" is a different problem from hiding "which user did what," and only the first is a network-layer problem.

Defending the application-layer graph is a **separate, harder project**: sealed-sender / metadata-minimization (unlinking the *sender* from the envelope so the service can route a message without learning who sent it), private contact discovery, oblivious/PIR reads, VRF-backed private key-directory lookups (the whitepaper already notes the last as an upgrade path for the transparency log, §6.9 limit (3)). Those change the *data model and access patterns*, not the transport.

**The relay and sealed-sender compose; neither substitutes for the other.**
- Relay **without** sealed-sender: the operator can't see your IP but still sees your entire keyed-by-`user_id` social graph. IP-anonymous, not graph-anonymous.
- Sealed-sender **without** relay: the operator can't tie an envelope to a sender in its data model, but still sees the source IP of the connection that delivered it — and can often re-link via IP. Graph-anonymous at the row level, but IP re-links it.
- You need **both** to claim meaningful metadata privacy against the operator. This doc scopes only the first. Building the relay must not create the impression the second is done.

### 2.3 What it does NOT defend: the global passive adversary

The overlay inherits Tor's most famous limitation. An adversary who can observe traffic at **both** ends of a circuit — near the user *and* near the first-party service — can correlate packet timing and volume and **de-anonymize the circuit**, regardless of how many hops it has. A nation-state with visibility into the user's ISP and into Cloudflare/Turso/LiveKit's ingress is exactly this adversary. Single-hop and multi-hop both fall to it; only heavy mixnet-style batching (§6.3, high latency) resists it, and even then only partially.

Therefore the claim we make is precisely:

> **"Unlinkability of your IP against the service operator (and against a network observer on one side)."**
> **NOT "anonymity against a nation-state."**

Any user-facing copy that says "anonymous" or "untraceable" is false and must be rejected in review. The correct framing is "the servers don't see where you're connecting from."

### 2.4 Defends/does-not summary

| Property | Defended by overlay? |
|---|---|
| Turso/LiveKit/DS learning your source IP | ✅ Yes (this is the point) |
| A local network observer / ISP learning *which service* you're talking to | 🟡 Partial — already TLS-wrapped; relay hides the destination host from a *local* observer if the first hop is to the relay, not the service. See §6. |
| Service operator learning your social graph / message metadata (by `user_id`) | ❌ No — application-layer, needs sealed-sender (§2.2) |
| A single malicious relay operator linking your IP to your activity | 🟡 Depends on architecture: single-hop **no**, multi-hop **yes** (§6) |
| Global adversary correlating both ends | ❌ No (§2.3) — Tor's known limit |
| Content confidentiality (messages, files, voice) | ➖ N/A — already E2EE by MLS; the relay carries ciphertext only (§8) |

---

## 3. The Relay Is NOT the Proof of E2EE

A framing error to pre-empt hard, because it is the most tempting piece of marketing and the most damaging if made.

**What actually proves Pollis isn't lying about end-to-end encryption** is that anyone can verify the *running code matches the published source*: verifiable / reproducible builds plus the existing append-only, Ed25519-signed **transparency logs** at `verify.pollis.com` (`docs/security-whitepaper.md` §6.9). A skeptic can replay the MLS commit log and the account-key directory and prove the server hasn't forked history or swapped a key, trusting only the log's pinned public key — not the server, not Turso, not the host (`docs/transparency.md`). *That* is the substrate of the E2EE claim: the crypto is in code you can audit, running as published.

The relay proves **none** of that. A relay forwards already-encrypted bytes. It says nothing about whether the client encrypted them correctly, whether the build matches source, or whether the server is honest about key history. A perfectly functioning relay in front of a backdoored client would give you private *metadata* delivery of a *broken* E2EE system.

> **The relay is metadata defense-in-depth. It is a *complement* to the E2EE + transparency story, layered underneath it. It is never the evidence for it.**

The specific overclaim to forbid: *"Your messages are safe because we route them through a peer network."* That sentence is false in the same way "Messenger is private because we have encryption in transit" is false — it points at the wrong layer to launder a confidentiality claim. Pollis's differentiator is that it doesn't do that. The relay must be described as exactly what it is: *"an optional layer that hides your IP address from our servers."* No more.

---

## 4. Threat Model

We enumerate adversaries and state, per adversary, what they learn with the overlay **on** vs **off**. "On" assumes the recommended phased target; where hop count matters we split it.

### 4.1 Honest-but-curious service operator (Turso / LiveKit / R2 / DS)

The operator runs the infrastructure, logs faithfully, does not actively attack, but *reads what it has*. This is the primary adversary the overlay targets.

| | Overlay OFF | Overlay ON |
|---|---|---|
| Source IP of the connecting user | ✅ Sees real IP (`whitepaper` §1.2) | ❌ Sees a relay's IP |
| Social graph / message metadata by `user_id` | ✅ Full (data model) | ✅ **Still full** — unchanged (§2.2) |
| Message / file / voice plaintext | ❌ Never (MLS) | ❌ Never |
| Timing of a user's activity | ✅ | 🟡 Sees timing of the *relay's* forwarded traffic; correlatable if the operator also controls/observes the relay's ingress (see §2.3) |

Net: the overlay converts "operator knows the household behind account `u_123`" into "operator knows account `u_123` is active but not from where." A real, if bounded, improvement against subpoena / breach / curiosity.

### 4.2 Network observer (ISP, local Wi-Fi, on-path AS)

Sees packets near the user but not near the service.

- **OFF:** sees TLS connections from the user to the Turso/LiveKit/R2/DS hosts. Content is encrypted, but the *destination* (hence "this person uses Pollis") and traffic *volume/timing* are visible via SNI/IP.
- **ON:** sees a TLS/QUIC connection from the user to a **relay** (a peer IP, not an obvious Pollis host). This hides *which Pollis service* the user is talking to from a local observer, and — if relay endpoints are not trivially fingerprintable — can obscure *that it's Pollis at all* to a degree. It does **not** defeat an observer who also watches the service side (§2.3). Traffic-analysis fingerprinting of Pollis's characteristic request pattern remains possible; padding is a v2 concern.

### 4.3 Malicious relay peer

A user (or attacker) volunteers as a relay and behaves adversarially.

- **Sees:** the ciphertext bytes it forwards, and the two endpoints of its hop. Because it forwards **already-TLS-encrypted first-party traffic to a known first-party host** (§8), it learns: *"some client (whose IP I see if I'm hop 1) is exchanging encrypted traffic with `turso-host`."* It does **not** learn message content, `user_id`, MLS state, or any key material — all of that is inside the TLS session to Turso/DS, which terminates at the *service*, not the relay.
- **Single-hop:** the relay **does** see the originating user's real IP (it is hop 1). So a single-hop relay is a *trusted* relay w.r.t. IP — it learns exactly the thing we're hiding from Turso. This is the central weakness of v0 and is called out explicitly in §6.1.
- **Multi-hop:** hop 1 sees your IP but not your destination-side identity; the last hop sees the destination but not your IP. No **single** relay links both. This is why v1 exists.
- **Can it tamper?** It can drop/delay/reorder bytes (availability attack) but cannot forge or read content: the TLS to the first-party service provides integrity + confidentiality end-to-end through the relay. Worst case it degrades or denies service on that circuit; the client detects a dead circuit and rebuilds through a different relay (or falls back — §7). It cannot silently corrupt an MLS commit: the DS and MLS layer would reject a mangled blob, and the client retries.

### 4.4 Sybil attacker (many relays)

An adversary runs a large fraction of the relay pool to raise the probability of controlling enough hops to de-anonymize.

- **Single-hop:** every relay already sees the IP of whoever it serves, so Sybil doesn't *add* capability beyond scale — but it does let a well-resourced adversary observe a large fraction of users' IP↔activity links. Mitigation: **first-party relays in the pool** (the fallback pool, §7) that the client always includes/prefers, so a purely-Sybil path is unlikely; plus relay selection that doesn't over-trust unknown peers.
- **Multi-hop:** the classic Tor Sybil concern — controlling both the entry and exit of a circuit. Mitigations are the standard ones: (a) **guard relays** (a client pins a small stable set of entry relays, biased toward first-party/known-good, so it doesn't re-roll the entry every circuit and eventually get a bad one); (b) path selection that requires hops in distinct trust domains / ASNs; (c) capping how much of the pool any one operator key can represent, with relay identity keys and possibly proof-of-first-party or vouching.
- **Structural mitigation unique to Pollis:** because the destination is always first-party, the operator can run a **known-good first-party relay tier** and *require the last hop to be first-party* (or first-party-attested). That means a Sybil adversary can never be the last hop, which removes half the both-ends attack for free. This is a genuine advantage of the closed model over Tor.

### 4.5 Global passive adversary

Covered in §2.3. Out of scope to fully defend; we do not claim to.

---

## 5. Non-Goals / Explicit Constraints

Stated up front because they bound the whole design:

1. **No general-purpose anonymity network.** Closed overlay to first-party endpoints only (§1). No exits, no arbitrary destinations, ever.
2. **No distributed application services.** *The user has explicitly ruled this out.* The overlay relays **transport bytes**; it does **not** distribute Turso, the DS, R2, or LiveKit onto peers. Peers never store metadata, never hold the commit log, never mint tokens, never become "the server." A **relay pool** (peers forwarding TLS bytes to the first-party server) is acceptable; **full service distribution** is not. This line is load-bearing and appears again in §7 and §11 — do not let a reliability or scaling argument erode it into "let peers cache/serve data." Peers forward opaque bytes to the one true first-party service; that is the entire remit.
3. **Not a substitute for E2EE, the transparency log, or sealed-sender** (§2, §3).
4. **Latency-critical media is not sacrificed for metadata** (§6.4). Pollis wants to be the fastest secure messenger; the overlay must not tax voice.

---

## 6. Architecture Options

Three points on the latency/privacy curve, plus the plane-splitting idea that makes any of them shippable given Pollis's speed goal.

### 6.1 Option A — Single-hop trusted relay (v0)

```
Client ──TLS(relay)──► Relay peer ──TLS(Turso/DS)──► first-party service
   \_________________ user's real IP seen by Relay only _____________/
                       service sees Relay's IP
```

The client opens an outer connection to **one** relay; the relay opens the inner connection to the first-party host and pipes bytes. The relay is effectively a SOCKS-to-allowlisted-host proxy.

- **Wins:** Turso/LiveKit/DS no longer see the user's IP — they see the relay's. Lowest possible added latency (one extra hop). Simplest to build. Delivers the §2.1 property against the *service operator*.
- **Costs:** the **relay sees your real IP** and which first-party host you're talking to. So you've moved the IP-linkage from Turso to the relay operator. This is only a win if the relay is *more trusted* or *less able to correlate* than the service — e.g. a **first-party-operated relay pool** (the relay operator is Pollis, but the relay tier is architecturally separated from the Turso-facing metadata and can be run so it doesn't log/join IPs to accounts), or a relay run by an org the user trusts more than the DB host. Against an honest-but-curious *service* operator who does **not** also run the relay tier, single-hop is a real win: no single system holds both your IP and your account-keyed activity.
- **When it's enough:** for the majority threat model ("I don't want the messaging DB's connection logs to pin my home IP to my account, and I don't want a DB breach to leak that"), single-hop to a relay tier that is operationally separated from the metadata store is a meaningful, cheap improvement. It is the recommended **v0**.

### 6.2 Option B — Multi-hop onion (v1)

```
Client ─►[Relay 1]─►[Relay 2]─►[Relay 3]─► first-party service
         sees IP,    sees only   sees dest,
         not dest     neighbors   not IP
```

Layered (onion) encryption: the client wraps the payload in one encryption layer per hop, keyed to each relay, so each relay can peel exactly one layer and learns only its predecessor and successor — never both the origin IP and the destination.

- **Wins:** **unlinkability even against a single malicious/curious relay operator.** No one relay knows both who you are (IP) and what you're doing (which first-party host). Defeats the §4.3 single-relay attack and much of §4.4 (given good path selection + first-party last hop, §4.4).
- **Costs:** each hop adds a full network RTT and a crypto layer. For the control plane (libSQL CRUD, DS submission) this is tolerable — those are already async, retryable, non-interactive (`messages.rs` send is fire-and-forget with offline catch-up per whitepaper §6.6). For anything interactive it's painful.
- **Build cost:** materially more than v0 — circuit construction, per-hop key agreement, onion encryption, path selection, guard relays. This is where "reuse a library" (§9) matters most.

### 6.3 Option C — Mixnet-style batching (v2)

Relays **batch and reorder** messages, adding deliberate delay and cover traffic so that *timing* correlation between a circuit's two ends is broken.

- **Wins:** the only option that meaningfully raises the bar against the §2.3 global adversary's timing correlation.
- **Costs:** seconds-to-minutes of latency by design. Acceptable *only* for the most latency-insensitive control-plane operations (e.g. background commit submission, key-package replenishment), never for anything a human waits on. Cover traffic burns bandwidth/battery.
- **Verdict:** almost certainly **over-scoped for Pollis's actual threat model** (a fast consumer messenger, not a whistleblower-grade anonymity tool). Keep it as a documented v2 lever for a *specific opt-in "maximum privacy" mode* if a real user need appears; do not build it speculatively.

### 6.4 Plane splitting — the key architectural move

The tension: **every hop taxes latency**, and Pollis explicitly wants to be "the fastest secure messenger" (media in Rust, direct Turso, "1 network hop — simple and fast," `ARCHITECTURE.md`). You cannot put real-time voice behind a 3-hop onion circuit and keep that claim.

Resolution: **split the traffic into two planes and treat them differently.**

- **Control plane (metadata-sensitive, latency-tolerant):** libSQL CRUD to Turso, MLS commit submission to the DS, key-package/welcome polling, R2 presign requests. These are the operations whose *IP metadata* is worth hiding (they're keyed to your account activity) and they are already **asynchronous and retry-tolerant** — a send fires a LiveKit wake and offline recipients catch up on next read (`whitepaper` §6.6). **Route this plane through the overlay.** An extra 50–150 ms per control operation is invisible to the user because these aren't in the interactive hot path.
- **Media plane (latency-critical, less metadata-sensitive):** LiveKit RTP/voice frames. These carry **already frame-level E2EE ciphertext** (AES-128-GCM via `FrameCryptor`, keyed by the MLS exporter secret — `whitepaper` §10.2), so the SFU already sees no plaintext. What it sees is RTP routing metadata + your IP. Voice is round-trip-latency-critical (interactive audio degrades badly past ~150–200 ms mouth-to-ear). **Keep media direct, or at most single-hop, and never onion-route it.** If a user in "max privacy" mode accepts degraded voice, that's an explicit opt-in, not a default.

**Rough latency budget** (order-of-magnitude, to size the trade, not a benchmark):

| Path | Added one-way latency | Notes |
|---|---|---|
| Direct to Turso/DS (today) | 0 (baseline) | `ARCHITECTURE.md`: "1 network hop" |
| v0 single-hop control plane | +1 RTT to relay (~10–40 ms typical, region-dependent) | Invisible for async CRUD/commits |
| v1 3-hop onion control plane | +3 RTT + 3× onion crypto (~40–150 ms) | Fine for async ops; not for interactive |
| Voice via overlay (**avoid**) | +1–3 RTT into the interactive budget | Breaks the fast-voice claim; media stays direct |

The design principle: **the user should never wait on the overlay.** The overlay lives on paths where an extra RTT is amortized by async delivery and offline catch-up. This is exactly why Pollis's async control-plane design (fire-and-forget send, poll-based catch-up) makes it *unusually well-suited* to a relayed control plane — the latency cost lands on operations that were never in the human's critical path.

---

## 7. Reliability: You Need a First-Party Fallback Relay Pool Regardless

Consumer peers are the opposite of reliable infrastructure: they **churn** (close the app, sleep the laptop), are frequently behind **NAT/CGNAT**, hop networks, and have asymmetric/limited uplinks. A messenger's core job is that **messages must work** (`CLAUDE.md`: "Messages must work. History is bounded, not flaky."). An overlay that can wedge delivery when peers are scarce is unacceptable.

**Therefore, regardless of how ambitious the peer layer gets, the overlay MUST include a first-party-operated fallback relay pool** — a set of always-on relays Pollis runs, that the client uses when no suitable peer relay is available (or always, in v0). This is not a compromise of the "no distributed app-services" constraint (§5.2): a **relay pool forwards TLS bytes to the one true first-party service**; it does not become the service. Reasserting the line: acceptable = "Pollis runs some relay nodes that proxy your bytes to Turso"; **not** acceptable = "peers or relay nodes store/serve metadata or the commit log." The fallback pool stays firmly on the acceptable side.

Design implications:

- **Availability floor:** if the overlay is enabled and no peer relay is reachable, the client uses a first-party relay (still IP-unlinking from *Turso* if the relay tier is operationally separated from the Turso metadata plane, §6.1). If even that is unreachable and the user has opted into "overlay required," the client must surface a clear degraded state — it must **not** silently drop a send (violates the messages-must-work invariant). A sensible default is "prefer overlay, fall back to **direct** on total overlay failure unless the user set strict mode."
- **Health / circuit rebuild:** dead-relay detection with fast failover to another relay or the fallback pool, mirroring the existing `RemoteDb::with_retry` reconnect discipline (`pollis-core/src/db/remote.rs`) — the client already knows how to transparently reconnect a dropped Hrana stream; overlay circuit failure should reuse that resilience posture.

### 7.1 NAT traversal

Most consumer peers can't accept inbound connections. Two existing assets to reuse rather than reinvent:

- **LiveKit's ICE/TURN infrastructure.** Pollis already ships a full WebRTC stack (libwebrtc via the `livekit` crate, `pollis-core/src/commands/voice.rs`) and already depends on LiveKit for signalling. WebRTC data channels do ICE (STUN for hole-punching, TURN for relay-of-last-resort) out of the box. A peer-relay transport built on **WebRTC data channels** would inherit NAT traversal essentially for free and reuse a battle-tested, already-shipped stack. TURN-relayed fallback is itself a first-party relay hop (Pollis runs the TURN server), which dovetails with §7's fallback pool.
- **libp2p hole-punching (DCUtR).** If the relay layer is built on `rust-libp2p` (§9), its DCUtR + AutoNAT + relay-v2 machinery is purpose-built for exactly this: NATed peers discover reachability, hole-punch when possible, and fall back to circuit-relay through a public node when not. This is the most "batteries-included" path for a peer-to-peer relay mesh, at the cost of a heavier dependency and a second networking stack alongside libwebrtc.

**Recommendation:** for **v0**, don't do peer-to-peer NAT traversal at all — v0 relays are the **first-party fallback pool** (publicly reachable, no NAT problem). Peer-hosted relays (which need NAT traversal) arrive with **v1**, and at that point prefer **reusing the WebRTC/ICE stack already in the tree** over adding libp2p, unless a multi-hop mesh's routing needs push you toward libp2p's relay/DHT primitives. Adding a whole second networking stack is a real cost to weigh (§9).

---

## 8. What a Relay Sees: Ciphertext to a Known Host, Nothing More

Spelled out because it is the safety argument for asking users to carry each other's traffic.

A relay peer **only ever forwards bytes that are already inside a TLS (or WebRTC/DTLS) session terminating at a first-party service.** The layering, hop by hop:

```
[ MLS ciphertext / already-E2EE payload ]        ← relay cannot read (no MLS keys)
  wrapped in TLS to Turso/DS/R2  (or DTLS-SRTP to LiveKit)   ← relay cannot read (session terminates at the service)
    wrapped in the relay-hop transport (TLS/QUIC/WebRTC to the relay)   ← this is all the relay terminates
```

- The relay terminates **only the outer relay-hop transport.** Inside it is an opaque TLS record stream destined for a first-party host. The relay forwards those bytes; it holds no key to them.
- The relay **never sees plaintext** of any kind — not message content (MLS-encrypted end to end), not `user_id`-level application payloads (those are inside the TLS-to-Turso session), not voice (frame-level AES-GCM *and* DTLS-SRTP), not attachment bytes (convergent-AEAD ciphertext even before TLS, `whitepaper` §9).
- The **most** a relay learns: *"a client is exchanging encrypted traffic with `<one of four known first-party hosts>`,"* plus — **only if it is hop 1** — that client's source IP. In multi-hop, no single relay gets both facts (§4.3).
- A relay **cannot inject or alter** content: the inner TLS to the service provides integrity end-to-end through the relay. Tampering degrades to denial (drop/delay), which the client routes around.

This is a genuinely strong safety story to give volunteers: **"If you run a relay, you are forwarding sealed envelopes to Pollis's own servers. You cannot read them, you cannot tell what's in them, and you are never the source of traffic to anyone but Pollis."** It is categorically safer than running a Tor relay (let alone an exit), and that difference is directly attributable to the closed-overlay design (§1).

---

## 9. Implementation Sketch in the Pollis Stack

### 9.1 Where the logic lives

`pollis-core` is a reusable Rust crate with **no shell-runtime dependency**, already consumed by desktop (`src-tauri`) and mobile (uniffi) (`ARCHITECTURE.md` §Project structure, `CLAUDE.md`). The overlay client belongs **here**, so desktop and mobile inherit it from one code path — the same rationale that keeps media in Rust. Concretely, a new module, e.g. `pollis-core/src/net/overlay/` (or a small sibling crate in the workspace if it grows a heavy dependency tree like libp2p, to keep `pollis-core`'s compile surface lean).

The relay **server** logic (the node that forwards bytes) is a separate small binary — most naturally a sibling to `pollis-delivery` in the workspace (e.g. `pollis-relay/`), deployed as the first-party fallback pool (§7). For peer-hosted relays (v1), the same server logic compiles *into* the client (`pollis-core`) behind the opt-in flag (§10), so a consenting user's app can act as a relay.

### 9.2 How it plugs into endpoint resolution (`config.rs`)

Today every service address is a field on `Config` (`config.rs:4-25`), resolved once at startup and handed to the connection builders — `RemoteDb::connect(url, token)` for Turso (`pollis-core/src/db/remote.rs`), the LiveKit room connect, R2 SigV4 requests, and the DS HTTP client. The clean insertion point is a **local overlay shim** that these connections dial *instead of* the real host:

1. **Local SOCKS-style shim.** The overlay client exposes a loopback proxy (`127.0.0.1:<auto-port>`), conceptually a SOCKS5-to-allowlisted-first-party-host proxy. This mirrors a pattern Pollis already uses and blesses: the *"Rust-side local-loopback HTTP server (`127.0.0.1:<auto-port>`)"* for media transport (`CLAUDE.md` §Performance Architecture). The overlay is the same shape for the control plane — a local proxy the rest of `pollis-core` dials.
2. **Endpoint re-resolution.** When the overlay is enabled, `Config` resolution (or a thin wrapper over it) rewrites the outbound path so that the libSQL/DS/R2 connections connect **through** the shim, which builds/selects an overlay circuit to the *real* configured host and pipes the (still end-to-end-TLS-to-the-service) bytes. The real `turso_url` etc. remain the *inner* destination; the shim is the *outer* dial target. Because `libsql` uses `rustls` over the connection (`whitepaper` §8), the TLS session still terminates at Turso — the shim only carries bytes.
3. **Per-plane routing.** The shim knows which inner host a connection targets and applies the §6.4 policy: control-plane hosts (Turso, DS) → overlay circuit; media host (LiveKit) → direct or single-hop only. This keeps the plane split in one place.
4. **Minimal blast radius.** The connection builders don't need to know the overlay exists — they dial a local address; the shim does the rest. This matches the codebase's "one URL pattern, no platform-branching" instinct (`CLAUDE.md`) and keeps `config.rs` as the single source of endpoint truth (§5-ish), with the overlay as a transport wrapper around it, not a fork of it.

### 9.3 Candidate transports

- **`rust-libp2p`.** Batteries-included for a peer mesh: transport (QUIC/TCP+Noise), relay-v2 (circuit relay), DCUtR hole-punching, AutoNAT, peer identity keys, Kademlia for relay discovery. Best fit if v1 multi-hop peer mesh is the real goal. **Cost:** a large second networking stack in the binary alongside libwebrtc; compile-time and binary-size hit; a lot of surface `pollis-core` doesn't have today.
- **WebRTC data channels (reuse the shipped LiveKit/libwebrtc stack).** Lowest *marginal* cost because the stack is already in the tree and already solves NAT (§7.1). Good fit for v0/v1 single- and few-hop relaying. **Cost:** WebRTC's connection setup is heavier per-circuit; onion-layering over data channels is more bespoke than libp2p's ready-made relay primitives.
- **Minimal custom relay (TLS/QUIC + a tiny framed protocol).** For **v0 first-party single-hop**, you arguably need *neither* of the above: a small QUIC (`quinn`) or TLS relay that accepts an authenticated client, enforces the destination allowlist, and pipes bytes to the pinned first-party host. Smallest, most auditable, most in the spirit of "bespoke and small because the destination set is closed" (§1). **Recommended starting point** precisely because it de-risks the plumbing (§9.2) without committing to a mesh stack.

### 9.4 Relay authentication (reuse what exists)

The DS already gates writes with **device-certificate-signature auth** (`pollis-delivery/src/lib.rs` "Write authentication"; `pollis-delivery/src/auth.rs`; and the broker reuses `crate::writes::gate`, `docs/secrets-broker.md` §Auth model). The relay can reuse the **same device-signature scheme** to (a) authenticate that a connecting client is a real Pollis device (anti-abuse, rate-limiting, anti-Sybil signal) and (b) let relays present a first-party attestation. No new auth scheme — the identity substrate (Ed25519 device keys, `user_device.mls_signature_pub`) is already there.

---

## 10. Opt-In and Incentives

### 10.1 Opting into *using* the overlay

A setting: **off (default) → prefer-overlay → strict-overlay.** Default off keeps today's fast direct path for users who don't need IP privacy and preserves the speed claim as the out-of-box experience. "Prefer" routes the control plane through the overlay with direct fallback on total failure (§7). "Strict" refuses direct control-plane connections (surfacing a clear degraded state rather than silently dropping — §7, messages-must-work). Media policy per §6.4 in all modes.

### 10.2 Opting into *being* a relay

A separate, explicit consent: **"Help other Pollis users by relaying encrypted traffic."** Because a relay's safety story is strong (§8 — you forward sealed envelopes to Pollis's own servers, never readable, never to third parties), this is a far easier ask than "run a Tor relay." Consent must state plainly: bandwidth is used, battery/data may be consumed (so gate to **Wi-Fi + power-connected** by default on laptops/mobile), and you are **never** carrying readable content nor originating traffic to anyone but Pollis.

### 10.3 The incentive is altruism + trust signal, not payment

There is no token, no payment, no reputation market (that path invites Sybil-for-profit and scope creep). The incentives are:

- **Altruism / mission alignment.** Pollis's user base self-selects for people who care about metadata privacy; contributing relay capacity is a concrete way to strengthen the network they rely on — the same social contract that sustains Tor relays and Signal's ethos, minus the exit-node risk that deters Tor volunteers.
- **The trust signal.** A visible "you're helping N users stay private this month" is a legitimacy and community signal that reinforces why someone chose Pollis. It aligns with the transparency ethos (`verify.pollis.com`) — participation is another way the community, not just the operator, underwrites the system's privacy.
- **Skin-in-the-game for power users.** Privacy-maximalist users get a stronger anonymity set *for themselves* by growing the pool (a larger, more diverse relay set makes §4.4 Sybil harder), so contributing is partly self-interested.

Safeguards so the incentive never distorts safety: a relay **cannot** gain any read access by relaying (it only ever sees §8's ciphertext-to-a-known-host); relay reputation must not become a data-access lever; and the first-party fallback pool (§7) guarantees the network works even if *zero* peers volunteer — so incentives grow the anonymity set but are never load-bearing for basic function. That last property is important: it means we never have to over-incentivize (and thereby invite abuse) to keep the product working.

---

## 11. Open Questions and Risks

1. **Does single-hop actually help against the real adversary?** v0's value hinges on the first-party relay tier being *operationally separated* from the Turso metadata plane (so no single system joins IP↔account). If Pollis runs both and can trivially join their logs, v0 buys little against a *malicious* operator (it still helps vs. breach, subpoena-to-Turso-only, and honest-but-curious). **Decide and document** the operational-separation commitment before shipping v0, or be honest that v0 is "breach/subpoena defense," not "malicious-operator defense." **→ Decided & documented in `docs/relay-operations.md`:** v0 is honestly scoped as breach/subpoena + IP-unlinking defense (the B-direction choice), and the concrete separation mechanism is **offline device-cert auth** — the relay verifies a connecting device's cert chain locally (`pollis-device-cert`, zero I/O), so a relay node holds no Turso/DS credentials and makes no metadata-plane query. Log-anchoring the account id and live revocation are stated v1 items.
2. **Traffic fingerprinting.** Even IP-hidden, Pollis's characteristic request cadence (poll intervals, commit sizes) may fingerprint the app/user to an on-path observer. Padding/cover traffic is a v2 lever; note the residual risk now.
3. **Anonymity-set size.** IP unlinkability is only as strong as the crowd you blend into. A small relay pool with few users offers weak anonymity (few candidates behind a relay's IP). Real value needs adoption; measure pool size and set expectations.
4. **Sybil economics** (§4.4). The "require first-party last hop" mitigation is strong but re-centralizes trust in the last hop — acceptable (it's a relay, not the service, §5.2) but worth stating.
5. **Abuse / DoS toward first-party services** (§1.2 caveat). A relay pool can concentrate or amplify traffic at Turso/DS. Reuse device-sig auth (§9.4) + rate limits; treat relays as first-class rate-limited clients.
6. **Battery/data on mobile.** Being a relay on a metered/battery device is user-hostile if mis-defaulted. Default relay-serving to Wi-Fi + power only.
7. **The scope-creep risk toward distributed services.** Every reliability discussion will tempt someone to "just let peers cache a little metadata." **Hold the line (§5.2):** peers forward bytes; peers are never the service. Put this in the design's acceptance criteria.
8. **Maintenance cost of a second networking stack** (§9.3) vs. reusing WebRTC. A libp2p mesh is powerful but is a large, long-lived dependency. Weigh against starting minimal-custom.
9. **Interaction with the secrets broker (#393/#437).** As LiveKit-token minting and R2 presign move server-side into the DS, more of the client's outbound calls funnel through the **DS** — which *concentrates* the metadata-sensitive control plane onto one host, making it an even better single target to put behind the overlay. This is synergy, not conflict: note it, and make sure the DS client dials through the shim (§9.2).
10. **Do we even want it?** The most important open question. The transparency log + E2EE already deliver Pollis's headline guarantees (§3). The overlay is *metadata* defense-in-depth with real latency, adoption, and maintenance costs. It is worth building **if** IP-metadata-vs-operator is a threat the target users actually have (activists, journalists, at-risk populations) — and worth deferring if the user base primarily values content confidentiality (already solved). This is a product decision the doc should force, not assume.

---

## 12. Recommendation

**Build it — but small, opt-in, phased, and never oversold.** Specifically:

**Do build (worth the cost):**
- **v0 — Single-hop, first-party-operated fallback relay pool, opt-in, control-plane only.**
  Delivers the real, shippable value: **IP unlinkability from Turso and the DS** for users who opt in, plus breach/subpoena resistance on connection metadata. It is mostly transport plumbing (§9.2), not a cryptography project, so it is the lowest-risk way to get a genuine privacy improvement into users' hands. Keep media (LiveKit) direct (§6.4). Reuse device-signature auth (§9.4). Commit to (and document) operational separation of the relay tier from the Turso metadata plane, or scope v0 honestly as breach/subpoena defense (§11.1).

**Build later, gated on demand and threat model:**
- **v1 — Multi-hop onion for the control plane, with peer-hosted relays.**
  Removes the single-relay trust assumption (§6.2), adds the anonymity set from real peers, needs NAT traversal (§7.1) and guard/path selection (§4.4). Build only once v0 has adoption and there's evidence the "don't trust even the relay operator" threat is real for the user base.
- **v2 — Timing defenses (padding / batching), as a distinct opt-in "maximum privacy" mode.**
  Only if a whistleblower-grade threat model materializes (§6.3). Almost certainly over-scoped for a fast consumer messenger; keep as a documented lever, not a roadmap commitment.

**De-risk first (prototype these before committing to v0):**
1. **The local SOCKS-style shim + endpoint re-resolution** in `pollis-core` (§9.2). This is the load-bearing, protocol-agnostic plumbing — prove that Turso's libSQL/Hrana session and the DS HTTP client survive being tunneled through a loopback proxy to the real host with acceptable added latency, on desktop *and* mobile. If this is clean, v0 is straightforward; if it's messy, it will dominate the effort, so learn it first.
2. **A minimal custom first-party relay** (`quinn`/TLS + destination allowlist, §9.3) rather than reaching for libp2p on day one. It's the smallest thing that validates the whole idea and matches the closed-overlay spirit.
3. **A latency measurement** of the control plane through one hop, to confirm the §6.4 budget holds and the plane-split is sufficient to protect the fast-voice claim.

**Framing guardrails (non-negotiable in any shipped version):**
- Market it as *"hides your IP from our servers,"* never as *"anonymous,"* *"untraceable,"* or — the cardinal sin — *the proof/reason messages are E2EE* (§3). The E2EE proof is verifiable builds + the transparency log; the relay is defense-in-depth for metadata only.
- Be explicit that it does **not** hide the application-layer social graph (§2.2); that's a separate sealed-sender project, and shipping the relay must not imply otherwise.
- Hold the "**relay pool, not distributed services**" line (§5.2, §11.7): peers forward sealed bytes to the one true first-party service; they never become the service.

**Bottom line:** the closed-overlay design is what makes this *tractable and safe* for Pollis in a way general onion routing never is — no exits, no third-party liability, a strong "you only ever forward sealed envelopes to Pollis" story for volunteers, and an async control plane that's unusually tolerant of an extra hop. The honest value is bounded (IP-vs-operator metadata, not anonymity, not the social graph, not the E2EE proof), but it is real and cheaply reachable at v0. Recommend prototyping the shim to de-risk, then shipping v0 opt-in — and deciding v1+ on evidence of actual demand.

---

## 13. Status & GitHub-issue summary

**Status: DEFERRED.** Sequenced after metadata minimization (sealed sender): a relay that hides IP while the app layer still transmits a plaintext sender column defends the weaker half of the metadata. Revisit trigger: sealed sender (metadata-minimization v1) shipped.

> **Title:** Opt-in closed-overlay relay — hide client IPs from the first-party services (Turso / DS / R2 / LiveKit)
>
> **Problem.** Every client connects directly to a fixed set of four first-party endpoints (§1.1), so Turso/LiveKit/DS see the source IP of every connection (§2.1, whitepaper §1.2) — pinning account activity to a network/household and leaving IP↔account links exposed to breach, subpoena, or operator curiosity. This is network-layer metadata only; the application-layer social graph is a separate sealed-sender project (§2.2).
>
> **Approach.** A **closed overlay, not Tor**: relays only ever forward opaque TLS bytes to the pinned first-party hosts (static allowlist, no exits, no third-party liability — §1). Control-plane traffic (Turso CRUD, DS commits) routes through the overlay; latency-critical media stays direct (§6.4 plane split). Implementation: a local SOCKS-style shim in `pollis-core` + endpoint re-resolution (§9.2), a minimal custom first-party relay (§9.3), device-signature auth reused from the DS (§9.4).
>
> **Phased milestones** (§12):
> - **v0** — single-hop, first-party-operated fallback relay pool; opt-in; control-plane only. De-risk first: prototype the shim + endpoint re-resolution, a minimal `quinn`/TLS relay with destination allowlist, and a one-hop latency measurement against the §6.4 budget.
> - **v1** — multi-hop onion for the control plane with peer-hosted relays (NAT traversal via the shipped WebRTC/ICE stack, guard/path selection, first-party last hop). Gated on v0 adoption and evidence of the "don't trust even the relay operator" threat.
> - **v2** — timing defenses (padding/batching) as a distinct opt-in "maximum privacy" mode. Only if a whistleblower-grade threat model materializes.
>
> **Acceptance criteria** (consolidated from §5, §7, §10, §11, §12):
> - Relays enforce a **static allowlist** of the four first-party hosts; no other destination is ever reachable (§1.2).
> - **Relay pool, not distributed services** (§5.2, §11.7): peers/relays forward opaque bytes only — they never store metadata, never hold the commit log, never mint tokens, never become the service.
> - Overlay use is **off by default**, with off → prefer-overlay → strict-overlay modes; strict surfaces a clear degraded state rather than silently dropping a send (§7, §10.1 — messages-must-work).
> - A **first-party fallback relay pool** exists so the network functions even with zero volunteer peers; incentives are never load-bearing for basic function (§7, §10.3).
> - Media (LiveKit) stays **direct or at most single-hop** in all modes; the §6.4 latency budget is confirmed by measurement before ship.
> - Being a relay is a **separate, explicit consent**, defaulted to Wi-Fi + power-connected (§10.2, §11.6); relaying grants no read access of any kind (§8, §10.3).
> - The **operational separation** of the relay tier from the Turso metadata plane is decided and documented before v0 ships — or v0 is honestly scoped as breach/subpoena defense, not malicious-operator defense (§11.1).
> - **Non-negotiable framing guardrails** (§3, §12): the relay is marketed as *"hides your IP from our servers"* — never as *"anonymous"* or *"untraceable"*, and **never as part of or proof of the E2EE guarantee** (that proof is verifiable builds + the transparency log); copy violating this is rejected in review. Shipping the relay must not imply the application-layer social graph (sealed sender) is solved (§2.2).

---

## 14. v0 Execution Plan — de-risk results, generic-CONNECT decision, surface, slices

*Added 2026-07-22 after the §12 de-risk spike. This section supersedes §9's "candidate"
hedging where it conflicts: the load-bearing seams are now **confirmed against the pinned crate
versions**, and the north-star direction is set (see below). §1–§13 remain the rationale; this is
the build contract.*

### 14.0 Direction: north star B + a browser/VPN stretch door (do not spec now, do not preclude)

The roadmap owner set the north star to **Option B (multi-hop onion, §6.2) as the real goal**, with
v0 (§6.1 single-hop first-party) as a **waypoint, not the destination** — because "trust us, we
operationally separate the relay tier from the metadata plane" (§11.1) is *unverifiable* and
contrary to Pollis's verify-don't-trust ethos. v1 removes that trust assumption; v0 ships the
plumbing and the honest breach/subpoena win in the meantime.

Additionally a **stretch goal** is declared **out of scope to spec today but must not be
architecturally precluded**: a future in-app browser ("extranet") or full-VPN mode where
medium-term *no client→Pollis-service traffic* is reliably associable and long-term *traffic to any
destination* is unassociable.

**The single binding rule this imposes on v0** (and the whole point of getting it right now, when
it is free): **the transport primitive is a generic anonymized stream — `CONNECT(host, port)` inside
the circuit. The first-party destination allowlist is enforced as relay-side *policy* (signed
config, checked at the last hop), never as *protocol structure*.** The client shim speaks a generic
CONNECT interface from day one; it does not know or care that today only the four first-party hosts
will be accepted. Consequences:
- A future webview/VPN consumer points at the *same* local shim and gets a stream — zero change to
  circuit crypto, framing, or the shim.
- "Extranet"/VPN later = widen the relay's signed egress policy (curated destinations), or bridge
  the last hop into **Tor** for arbitrary destinations. Running our *own* open exits reintroduces
  Tor's exit-node liability (§1.2) and yields only a weak Pollis-sized anonymity set — so the
  Tor-bridge is the documented candidate for "any destination", **not** a committed own-exit fleet.
- This costs v0 nothing: generic framing instead of a hardcoded 4-host proxy is the *same amount of
  code*, just factored as policy-over-primitive.

### 14.1 De-risk results (§12 item 1–2) — the load-bearing plumbing is GREEN

The §12 worry was "does libSQL's Hrana/TLS session survive being tunneled to the real host?"
**Retired by construction against the pinned deps:**

- **libsql 0.9.30** exposes `Builder::new_remote(url, token).connector(C)`
  (`database/builder.rs:770`). `C` becomes a `ConnectorService = Service<http::Uri, Box<dyn
  Socket>>` (`util/http.rs`). We supply `hyper_rustls::HttpsConnector<ProxyConnector>`: **rustls
  uses the request URI's host for SNI + certificate verification, so TLS still terminates at the
  real Turso** — the inner `ProxyConnector` only changes *where the TCP lands* (it dials the local
  shim / issues CONNECT). **No libsql fork.** The `libsql` version must stay pinned; a bump that
  drops `.connector()` is a breaking change to guard in CI.
- **reqwest 0.12.28** has native `Proxy` support incl. `socks5h://` (proxy-side DNS) behind the
  `socks` feature (currently OFF — one-line enable in `pollis-core/Cargo.toml`), plus built-in
  HTTP-CONNECT. Point every client at `socks5h://127.0.0.1:<shim-port>`.

**Chosen shim shape: a local SOCKS5 server on `127.0.0.1:<auto-port>`** (loopback, ephemeral port —
same blessed pattern as `media_server.rs`). SOCKS5 is the generic-CONNECT primitive §14.0 requires
and is natively consumable by reqwest *and* a future webview; the libsql connector speaks SOCKS5 to
the same port. Upstream of the shim (shim→relay) is our own framed protocol over QUIC (`quinn`) with
device-signature auth (§9.4); v0 circuit length = 1 hop, but the circuit abstraction takes `n` hops
so v1 is a policy/param change, not a rewrite.

### 14.2 Full client egress surface + v0 per-host routing

Every outbound path in `pollis-core` (+ shells), from the surface audit:

| Egress | Crate / site | Proxy seam | v0 routing |
|---|---|---|---|
| Turso reads (libSQL/Hrana) | `libsql` — `db/remote.rs:43` | `.connector()` ✅ | **overlay** |
| DS writes/reads | `reqwest` — `mls/ds_client.rs:135,388,421,444` | `.proxy()` ✅ | **overlay** |
| R2 presigned PUT/GET/DELETE | `reqwest` — `r2.rs:761,777,788` | `.proxy()` ✅ | **overlay** (R2 host allowlisted) |
| Transparency fetch (async) | `reqwest` — `transparency.rs:347,370` | `.proxy()` ✅ | overlay |
| Transparency **verify** | `ureq` (sync, in `verifiable-log-serve`) | ❌ none | **residual leak (§14.4)** |
| Push register (Expo) | `reqwest` — `push.rs:139` | `.proxy()` ✅ | see §14.4 (non-first-party host) |
| LiveKit signaling (WS) + media (RTP) | `livekit` crate | ❌ none | **direct** — plane split (§6.4) |

The 10 bare `reqwest::Client::new()` sites confirm there is **no shared HTTP client builder** today.
v0's first refactor is a `state`-aware `http_client()` helper that all sites call; it applies the
proxy when the overlay is on and is a no-op passthrough when off. This both wires the overlay and
removes the per-call-`Client::new()` anti-pattern (connection-pool win for free).

### 14.3 v0 slices (each independently reviewable + headless-gated)

- **Slice 1 — the shim + plumbing + a loopback test relay (the de-risk made real).**
  `pollis-core/src/net/overlay/`: (a) SOCKS5 loopback shim; (b) `Circuit` abstraction (n-hop,
  n=1); (c) the QUIC relay-client with device-sig auth; (d) routing policy `off | prefer | strict`
  + per-host plane table (§6.4); (e) the libsql `ProxyConnector` + the shared `http_client()`
  reqwest helper; (f) endpoint re-resolution wiring at `db/remote.rs` + the reqwest sites, gated by
  a new `Config` field (`POLLIS_OVERLAY`, default **off** → today's path byte-for-byte unchanged).
  A minimal `pollis-relay/` **lib+bin** (quinn, allowlist, device-sig, CONNECT framing, pipe) so
  tests spin it in-process. **Gate:** headless `-p pollis --no-default-features` build clean +
  tests: reqwest GET and a libsql-shaped TLS connection both tunnel through the shim→relay to a
  local test **TLS** server with the cert verified for the *real* name (proves end-to-end TLS
  survives); allowlist-rejection test; strict-mode surfaces degraded state (never silent-drops,
  messages-must-work); off-mode is byte-identical to direct.
- **Slice 2 — deploy shape of the first-party relay pool + `off→prefer→strict` UI + consent.**
  The `pollis-relay` binary as a deployable (see §7); the settings surface (§10.1) and the
  be-a-relay consent (§10.2) are UI and can follow once the transport is proven.
  - **Slice 2a (landed).** Hardened `pollis-relay` into a deployable first-party node:
    **production auth = the OFFLINE device-certificate chain** (the mechanism that keeps the relay
    tier out of the metadata plane, §11.1) — extracted into the shared `pollis-device-cert` crate so
    `pollis-core` (mints) and `pollis-relay` (verifies) share one frozen format with no crate cycle;
    the handshake now carries `account_id_pub` + `device_cert` + `identity_version` + `issued_at`
    (protocol v2), replacing the Slice-1 in-memory key resolver. Plus: TOML config file, generated /
    persisted QUIC identity, graceful shutdown (drain on SIGTERM/SIGINT), and per-account / per-IP
    rate + concurrency limits (`Rejected(RateLimited)`). Operational-separation commitment written up
    in **`docs/relay-operations.md`**. Bootstrap/OTP traffic (a device with no cert yet) cannot
    cert-authenticate and stays DIRECT — documented, mirrors the DS session-vs-device split.
- **Slice 3 — one-hop latency measurement** against the §6.4 budget (de-risk item 3), on the real
  control plane, before any "ship v0" call.

v1 (multi-hop, peer relays, NAT traversal via the shipped WebRTC/ICE stack, guard/path selection,
first-party last hop) builds on Slice 1's `n`-hop `Circuit` — no re-plumb.

### 14.4 Residual leaks to state honestly in v0 (do not paper over)

1. **`ureq` transparency-verify path can't proxy** (`verifiable-log-serve`, sync). The account-key /
   build-verify *fetch* has an async reqwest path we proxy; the *verifier* uses ureq and will leak
   the client IP to `verify.pollis.com`. Options: route verify through the shim via ureq's
   (limited) proxy env, port the verifier fetch to reqwest, or accept + document. v0 **documents**
   it; it is public-key fetching, not account-keyed activity, so it is the lowest-value leak.
2. **Push register (Expo `exp.host`) is not a first-party host.** It is outside the closed
   allowlist by definition, so it **cannot** go through the overlay (a relay would refuse it, per
   §1.2). v0 keeps it direct and notes it; longer term the DS should proxy push registration
   server-side (same shape as the #393 secrets-broker consolidation, §11.9) so the client stops
   talking to Expo directly at all.
3. **LiveKit signaling stays direct in v0** (no proxy seam in the `livekit` crate; media is direct
   by plane-split anyway). Routing LiveKit's WebSocket is a v1+ item requiring either an upstream
   connector seam or a wrapping shim.
