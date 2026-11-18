# Bots (#1182)

A bot is a **user** (`users.kind = 'bot'`) scoped to **one group** for its whole life; its **hosts are its devices** (`user_device` rows under the bot, each with its own cert, leaf and key-package pool). Epic and settled design: #872. Schema: `pollis-schema/migrations/000052_bots.sql` (see [Database](./database.md)). DS: `pollis-delivery/src/bots.rs`. Wire types: `pollis-api/src/bots.rs`.

## What the schema refuses
Triggers, so no DS code is in the path (tests: `pollis-delivery/tests/bot_schema.rs`): a bot user without its `bot` row, its membership or its exact address `bot-<id lowercased>@bot.invalid`; a human with a bot row or any `bot.invalid` address; `kind` changing; a bot in another group, as admin, as group owner, in a DM, invited, asking to join, or holding a push token; a bot's membership or `bot` row deleted while the bot exists; a group deleted while it has bots; a trigger claim for a non-bot.

**Order.** Create: `bot` → `group_member` → `users` → `user_device`. Delete: `users` → `bot` → everything else (`bots::delete_bot_in_tx`, used by `/v1/bots/delete`, a bot's own `/v1/account/delete`, and `teardown::purge_group`).

## DS endpoints
| Path | Auth | Purpose |
|---|---|---|
| `/v1/bots/pairing/request` | none, `probe` tier, 10 000 open max | Host files a request (ephemeral X25519 key, code lookup hash, poll verifier, device id; `NewBot{account_id_pub}` or `ExistingBot{bot_user_id}`); TTL 10 min |
| `/v1/bots/pairing/lookup` | device-signed, admin of `group_id`, `probe` tier | Find an open request by the typed code's hash; an add-host request only through its bot's group. The client re-derives the code from the returned ephemeral key |
| `/v1/bots/pairing/status` | poll secret | Host awaits `pending` / `created{bot_user_id, group_id}` / `wrapped{…, wrapped_account_key}`; unknown, expired, wrong secret all 401 |
| `/v1/bots/create` | device-signed admin | One transaction: bot row, membership, user (key into `account_key_log`), host device (no cert yet), security events on admin and bot. Cap `MAX_BOTS_PER_GROUP` = 25 → `limit_reached` |
| `/v1/read/bots` | device-signed admin | Bots of a group with hosts (`certified`, `revoked_at`) and the limit |
| `/v1/bots/delete` | device-signed admin | Account deletion of the bot, then leave wake-ups and the owed commit-log purge |
| `/v1/bots/hosts/approve` | device-signed current admin of the bot's group | Store the admin's signed host approval (opaque to the DS) on the bot's open add-host request; inbox nudge `bot_host_approval` |
| `/v1/bots/hosts/pending` | device-signed bot host | Approved, unwrapped add-host requests for the signer's bot |
| `/v1/bots/hosts/wrap` | device-signed bot host | First wrap wins (status CAS); approver re-checked as a current admin; registers the new host's `user_device` row |
| `/v1/bots/triggers/claim` | device-signed bot host, own installation | `(installation_id, trigger_key)` PK: first claim wins, the winner's retry wins again; kept 7 days |

After `created`/`wrapped` the host publishes its cert through `/v1/auth/publish-device-cert` (cert-validity gate, `user_id` in the body) and then its key packages, device-signed.

Removing a host is `/v1/devices/revoke` with `user_id` = the bot: allowed to any current admin of its group (`account::revoke_owner`), security event on both.

## DS refusals for a bot (tests: `pollis-delivery/tests/bots.rs`)
No OTP issued or verified for a `bot.invalid` address; no email change (403 for a bot, silently nothing to a `bot.invalid` target); no directory hit by username or email (`user_by_identifier` matches `kind = 'human'`); no DM create/add either direction; no group create; no invite, invite redemption or join request; no admin role, removal or leave (only deletion); no push token; no device link create/accept; no identity rotation or reset; key-package claims on a bot only by admins of its group. Reads never return a bot's email. `GroupMemberWire` and `UserWire` carry `is_bot` for the Bot badge (default false for older DSes).

## Teardown
A group left with no people (leave, or the last person's account deletion) is torn down with its bots; ownership and admin handoff never pick a bot. Account deletion of an admin blanks `bot.created_by`.
