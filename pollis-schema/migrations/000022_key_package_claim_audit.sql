-- Durable claim budget for `POST /v1/key-packages/claim`.
--
-- WHY. Claiming a key package is inherently cross-account — it is how you add
-- someone to a group — so the endpoint deliberately binds nothing from the body
-- to the signer, and until now it was deliberately unlimited too ("KP exhaustion
-- is a known concern tracked for #419"). Unlimited is the whole attack: one
-- authenticated account can drain every unclaimed package a target has published
-- by claiming in a loop. A device whose pool is empty cannot be added to a group
-- at all until it replenishes, and the packages are gone for good (a claim is a
-- one-way flip of `claimed`), so a stranger can hold an arbitrary user out of
-- every conversation they are invited to.
--
-- An in-memory counter is not a bound here for the same reason it was not one
-- for invite-link redemption (#847): the DS restarts on every deploy, and a
-- bound a rolling restart clears is not a bound. So the claims are recorded, and
-- the budget is counted from the table — it survives a restart and is shared
-- across instances.
--
-- It doubles as the audit trail for "who drained this pool", which is the
-- question an operator asks when a user reports they cannot be added anywhere.
--
-- Additive and backward-compatible per CLAUDE.md: one CREATE TABLE and two
-- CREATE INDEXes. Nothing shipped reads or writes it.
CREATE TABLE IF NOT EXISTS mls_key_package_claim (
    id               TEXT PRIMARY KEY,
    -- The authenticated account that claimed. On the DS's no-auth (dev) path
    -- there is no signed identity, so nothing is recorded and nothing is
    -- counted — that path already trusts whoever asks.
    claimer_id       TEXT NOT NULL,
    -- The account whose pool was drawn from.
    target_user_id   TEXT NOT NULL,
    -- The specific device, when the claim named one. NULL for a user-scoped
    -- claim.
    target_device_id TEXT,
    claimed_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The per-(claimer, target) budget: "how often has THIS account drawn from THIS
-- pool recently".
CREATE INDEX IF NOT EXISTS idx_kp_claim_pair
    ON mls_key_package_claim (claimer_id, target_user_id, claimed_at DESC);

-- The per-target budget: "how fast is this device's pool draining, from all
-- claimers at once". A single attacker is caught by the pair budget; a handful
-- of accounts sharing the work is caught by this one.
CREATE INDEX IF NOT EXISTS idx_kp_claim_target
    ON mls_key_package_claim (target_user_id, target_device_id, claimed_at DESC);
