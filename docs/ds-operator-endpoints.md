# DS operator endpoints

Two of the Delivery Service's `POST /v1/...` endpoints exist for a **human with a
device**, not for the app. Nothing in `pollis-core` calls them, and nothing should:
they are recovery and maintenance levers, driven by hand when something has gone
wrong or when an operator wants a sweep to run now rather than on its own cadence.

This file exists because "no client caller" is otherwise indistinguishable from
"dead code somebody forgot to delete" — which is exactly how #925 found them.

## How this stays true

Each endpoint is declared `Audience::Operator` in `pollis-api`'s `endpoints!`
table, and that classification is load-bearing in two directions:

- **No client caller can appear.** `ds_post` and every one of its variants require
  `ClientRequest`, which the table implements only for `Audience::Client` rows.
  Calling one of these from `pollis-core` does not compile.
- **No operator endpoint can go undocumented.** `every_operator_endpoint_is_documented_in_the_runbook`
  (in `pollis-api/src/lib.rs`) fails the build if an `Operator` row's path is not
  mentioned in this file.

So the two ways this drifts — someone quietly wires a client call, or someone adds
a third operator endpoint and tells nobody — are both build failures rather than
things a reviewer has to notice.

If one of these ever gains a legitimate client caller, flip its row to `Client`
and delete its section here. If one stops being useful, delete the row, the
handler and the section together.

## Authentication

Both are ordinary device-signed writes: the same four `X-Pollis-*` headers every
other DS write carries (`docs/security-whitepaper.md`, `pollis_delivery::auth`).
There is no separate operator credential — an operator drives them from a device
that is a member of the conversation in question, which is also what bounds what
they can reach. `POLLIS_DS_METRICS_TOKEN` gates the read-only metrics/config
routes and has nothing to do with these.

Because the signature covers `sha256(body)`, these are not one-liner `curl`s. The
practical way to send one is a small signing script or a debug build of the client
with the body pasted in.

---

## `POST /v1/welcomes/resubmit`

Re-arm a single MLS Welcome for a recipient device that never got it.

**Body** — `pollis_api::writes::ResubmitBody`:

| field | meaning |
|---|---|
| `conversation_id` | the group whose Welcome is being re-sent |
| `generation` | suite generation of that group (absent → `0`, the pre-hybrid lineage) |
| `recipient_id` | the user the Welcome admits |
| `recipient_device_id` | the specific device |
| `welcome` | TLS-serialized MLS Welcome, base64 (STANDARD) |

**Answers** `{"status":"ok"}`.

Idempotent. It upserts on the `(conversation_id, recipient_id, recipient_device_id)`
unique index (commit-log DB migration `000002`, #430 P2), so a re-send refreshes the
blob and sets `delivered = 0` rather than stacking a duplicate row.

**When to reach for it.** A device is in the group's roster, is not revoked, and
still cannot decrypt anything — the normal repair path is for a member to run a
reconcile, which re-adds the device and issues a fresh Welcome through
`/v1/commits`. This endpoint is the manual version of that last step for the case
where the tree is already correct and only the Welcome row is missing. It does not
add anyone to an MLS group; it only re-delivers a Welcome an operator already
holds.

**What it cannot fix.** If the device is not in the MLS tree, a resubmitted Welcome
is not the problem — nothing here creates membership.

## `POST /v1/envelopes/gc`

Run the watermark-gated envelope sweep for ONE conversation, now.

**Body** — `pollis_api::messages::EnvelopeGcBody`:

| field | meaning |
|---|---|
| `conversation_id` | the conversation to sweep |
| `is_dm` | `true` → DM cleanup query, `false` → group-channel cleanup query |
| `actor_id` | no-auth fallback for the acting user; ignored when auth is enforced |

**Answers** `{"status":"ok"}`.

**This is not the primary GC path, and has not been since #689.** The DS runs
`sweep_envelope_gc` over every conversation holding envelopes on a fixed cadence
(`POLLIS_DS_GC_SWEEP_SECS`, default 3600s), so collection no longer rides any
member's ingest and a conversation whose members all go quiet is still swept. That
is why no client calls this: there is nothing for a client to trigger.

It shares the one cleanup chokepoint with the sweep, so it applies exactly the same
rule — a row is deleted only when every non-revoked, live device of every current
member has watermarked past its `sent_at` (invariant **I3**,
`docs/backend-core-invariants.md`; retention policy in
`docs/metadata-retention-policy.md`). It cannot delete an envelope the sweep would
keep, which is what makes it safe to run by hand at any time.

**When to reach for it.** Verifying retention behaviour on one conversation without
waiting up to an hour for the next sweep, or forcing collection after fixing
whatever was pinning a watermark. If it collects nothing, the answer is a watermark
that has not advanced — look at the conversation's devices, not at this endpoint.
