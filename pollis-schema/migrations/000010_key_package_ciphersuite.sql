-- Post-quantum hybrid MLS (#454, phase P1b) — make a key package's ciphersuite
-- a queryable property, and give a device somewhere to advertise that it can
-- take part in a hybrid group. Both columns are inert on landing: no client
-- writes a non-default value until P2. This migration only creates the seams.
--
-- Additive + backward-compatible (CLAUDE.md migration rule): two ADD COLUMNs,
-- both NOT NULL with a DEFAULT, so every existing row is correctly labelled the
-- instant the migration runs and a previously-shipped app — which mentions
-- neither column in its INSERTs — keeps working unchanged.

-- The MLS ciphersuite code point of the stored KeyPackage. Default 1 = 0x0001 =
-- MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519, which is what every row in
-- existence today is, and what every currently deployed client publishes.
--
-- This has to be a COLUMN rather than something derived on read: `key_package`
-- is an opaque TLS-serialized blob, so the Delivery Service cannot filter a
-- claim by suite without parsing MLS structures it has no business parsing. The
-- value is client-supplied and therefore a ROUTING HINT only — mistagging can at
-- worst cause a failed add (the real suite is inside the blob and MLS itself
-- rejects a mismatch at validation), never a security bypass.
--
-- Claims narrow on (user_id, [device_id,] ciphersuite, claimed = 0), so the
-- classic and hybrid pools cannot contaminate each other: a hybrid claim against
-- a classic-only device returns "no key package" — the existing normal
-- control-flow outcome — rather than silently handing back a classic package
-- that would downgrade the group.
ALTER TABLE mls_key_package ADD COLUMN ciphersuite INTEGER NOT NULL DEFAULT 1;

-- Whether this device can participate in a post-quantum hybrid group. Default 0
-- = classic only, which is true of every device today. Nothing sets it to 1 in
-- this phase; it exists so P2/P4 can enforce "a hybrid group may never contain a
-- member without a hybrid KeyPackage" as a precondition the DS can check,
-- instead of discovering the mismatch when an Add commit fails.
ALTER TABLE user_device ADD COLUMN pq_capable INTEGER NOT NULL DEFAULT 0;
