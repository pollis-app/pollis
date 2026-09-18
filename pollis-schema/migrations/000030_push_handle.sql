-- #1122: opaque per-notification lookup handles, so the push payload stops
-- carrying `conversationId` and `kind` to Expo/APNs/FCM.
--
-- The payload was content-free but it named the conversation, which is exactly
-- the "which conversation, when" signal the metadata-minimisation design tries
-- to withhold — disclosed to three third parties on every message, all of them
-- outside the overlay by design.
--
-- The DS mints a random handle per notification, stores the mapping here, and
-- puts only the handle in the payload; the client resolves it over its existing
-- authenticated channel (`POST /v1/push/resolve`).
--
-- WHY A RANDOM HANDLE RATHER THAN AN HMAC OF THE CONVERSATION ID: an HMAC is a
-- stable pseudonym per conversation. The providers could not name it but could
-- COUNT it, and "this handle fires 40 times a day" is most of what the metadata
-- was worth. A fresh handle per notification leaves nothing to count.
--
-- GRANULARITY is one row per (message, recipient user), not per device. All of a
-- user's devices share a handle, so a provider cannot correlate across users;
-- correlating one user's own devices reveals nothing the authenticated resolve
-- does not already assume.
--
-- The honest cost: the DS gains a short-lived, explicit record that a
-- notification for conversation X went to user Y. It already knew that when it
-- built the payload — this makes it durable for the TTL, which it was not
-- before. That argues for a short TTL and a real sweep, not a different design.
CREATE TABLE IF NOT EXISTS push_handle (
    handle          TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    kind            TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The sweep predicate, and the scoped lookup `/v1/push/resolve` performs
-- (handle AND user_id, so a handle that is not yours is indistinguishable from
-- one that does not exist).
CREATE INDEX IF NOT EXISTS idx_push_handle_created ON push_handle(created_at);
CREATE INDEX IF NOT EXISTS idx_push_handle_user ON push_handle(user_id, handle);
