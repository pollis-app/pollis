-- A per-conversation counter the DS bumps whenever an account or device leaves
-- an MLS group's roster (#1874): account deletion, leaving or removal, device
-- revocation, identity reset.
--
-- Keyed by the MLS group id: the group id for a group's channels, the DM id
-- for a DM. A missing row reads as version 0.
--
-- `POST /v1/conversations/catch-up` returns it when asked
-- (`want_membership_version`). A sender that sees it change since its last
-- check re-runs its pre-seal reconcile at once instead of waiting out the
-- check interval, so the next message is sealed only after the departed
-- devices' leaves are removed.
--
-- Holds no user or device id. Deleted with its conversation.
--
-- Additive and backward-compatible: one CREATE TABLE. Shipped clients never
-- ask for it.
CREATE TABLE IF NOT EXISTS conversation_membership_version (
    conversation_id TEXT PRIMARY KEY,
    version         INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
