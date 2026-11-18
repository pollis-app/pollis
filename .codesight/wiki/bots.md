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

## Runtime
`pollis-bot` (crate `pollis-bot/`, install/run: `pollis-bot/README.md`) hosts bot installations and runs their **job loop**. Headless, on `pollis-core`, dials out to the DS only, survives restarts (installations reopen unattended from their keystore slot; the first job after start catches up on everything missed).

**Installations.** One runtime hosts many (several bots, several groups). Each has its own directory `<data dir>/installations/<id>/` (`installation.json` + its own encrypted store), its own keystore slot and its own `AppState`; nothing is shared. The installation id is the bot's user id while a bot has one installation. Limit: `max_installations` in `<data dir>/config.json`, **default 8**; `run` refuses to start past it and `pair`/`add-host` refuse to add past it (`installations::check_capacity`).

**Job loop** (`pollis-bot/src/job.rs`, one per installation):
1. Trigger: a wake-up (realtime new-message / membership / reconnect event for the group, or the start of the runtime) or a webhook call. Never a timer.
2. Take the installation's lock (`Installation::run_job`; one job at a time per installation, installations run in parallel). Wake-ups that arrive before a queued job starts catching up fold into it.
3. Catch MLS up through the normal ingest path (`InstallationCore::catch_up`); messages newer than the stored cursor become message triggers, oldest first. The bot's own messages are never triggers.
4. The installation's `Handler` returns `Action`s. Today: the built-in alerts handler. #2004 adds the WASM App as a second `Handler` impl; the loop does not change.
5. A trigger with actions is claimed on the DS (`/v1/bots/triggers/claim`, `(installation_id, trigger_key)`); a lost claim discards the actions. Trigger keys: `msg:<envelope id>`, `hook:<delivery id>` (later `req:<request id>`, `slot:<slot time>`); the kind prefix keeps a sender-chosen delivery id from posing as an envelope id.
6. Each action is checked against the grants (hard-coded in #1182: `post` to a text channel of the bot's own group, by id or name) and applied through the normal durable send path. Refused actions are reported, never applied; a handler error discards every action and claims nothing.
7. Release the lock.

A message's cursor advances once its outcome is final (posted, claimed elsewhere, refused, handler failed). A transient failure (claim or send unreachable) stops the job at that message without advancing, so the next wake-up retries it in order. Posted message ids are derived from `(installation, trigger key, action index)` (`job::action_message_id`), so a retry re-sends the same message rather than a second one, and the winner's repeated claim answers `won` again.

**Alerts webhook** (`webhook.rs`). Off until configured (`config.json` `"webhook": {}` or `run --webhook [ADDR]`); binds `127.0.0.1:8787` unless told otherwise. `POST /hooks/<installation_id>` with `{"channel", "text"}`, authenticated by `Authorization: Bearer <secret>` or `X-Pollis-Signature: sha256=<hex HMAC-SHA256(secret, body)>` (a present signature must verify; a bearer cannot rescue it). The per-installation secret (`pbw_…`) is generated on the first start with the webhook on, kept in the installation's keystore slot, printed once; `pollis-bot rotate-secret` replaces it. Optional `X-Pollis-Delivery` names the delivery: send the same one to every host of a bot and exactly one posts. Order of checks: unknown installation 404, bad auth 401, **rate limit 429 + `Retry-After` (per installation: burst 10, refilled at 30 a minute; `webhook.burst`/`webhook.per_minute`)**, malformed 400. Answers: `posted` 200, `claimed_elsewhere` 200, `refused` 422, `failed` 5xx with a fix, `accepted` 202 if the job takes over 30 s (it still finishes). Body cap 64 KiB; `post` text cap 4000 characters. `pollis-bot post` signs and sends to this same endpoint.

**State and logs.** Every line is scoped to the runtime or one installation; every failure ends with one fix (`JobError::fix`). States: pairing → waiting for approval → joining → (run) starting → catching up → connected, or job failed with the error and fix. `run` writes `<data dir>/status.json` on every state change (not on a timer); `pollis-bot status` reads it.

**Tests.** `cargo test -p pollis-bot`: the loop over a fake core with a shared fake DS (`tests/job_loop.rs`: lock exclusivity, handler-error discard, grant refusal, claim lost, two hosts one post, retry without skipping), the webhook (`webhook.rs` unit tests: bad secret, bad HMAC, unknown installation, rate limit, malformed), the runtime and subcommands end to end over a fake host (`tests/runtime.rs`, `tests/app.rs`), the binary's command line (`tests/cli_args.rs`).
