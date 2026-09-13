-- #1090: bind a push token to the device that registered it.
--
-- `push_token.token` is the primary key and the conflict branch reassigned
-- `user_id` unconditionally, so anyone holding a victim's Expo token string
-- could register it to their own account: the victim's device then received the
-- attacker's notifications and stopped receiving its own.
--
-- Simply refusing every reassignment would break the legitimate case the
-- original design was serving — switching accounts on the same phone, where the
-- same install re-registers the same token under a new user. The two are
-- distinguishable by WHICH DEVICE is asking, so the row records it. The value is
-- taken from the server-verified `X-Pollis-Device` of the signed request, never
-- from the body, so it cannot be spoofed.
--
-- Nullable and additive: rows written before this migration have no binding, and
-- the next re-register from their real device adopts it (first write wins). A
-- shipped client needs no change — it never sent a device id and still doesn't.
ALTER TABLE push_token ADD COLUMN device_id TEXT;

-- The ownership check reads (token, device_id) on every register.
CREATE INDEX IF NOT EXISTS idx_push_token_device ON push_token(device_id);
