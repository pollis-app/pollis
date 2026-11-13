-- Durable claim budget for `POST /v1/key-packages/claim`.
--
-- WHY. Claiming a key package is inherently cross-account — it is how you add
-- someone to a group — so the endpoint binds nothing from the body to the
-- signer. Each claim is recorded here, and the DS counts its claim budget from
-- this table, so the budget survives a restart and is shared across instances
-- (an in-memory counter would reset on every deploy, as with invite-link
-- redemption, #847). This keeps each device's published pool available for the
-- groups it is actually invited to.
--
-- The table is also the audit trail an operator reads when a user reports they
-- cannot be added to a group.
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

-- The per-target budget: "how fast is this device's pool being drawn, from all
-- claimers at once". The pair budget bounds one claimer; this one bounds the
-- total across claimers.
CREATE INDEX IF NOT EXISTS idx_kp_claim_target
    ON mls_key_package_claim (target_user_id, target_device_id, claimed_at DESC);
