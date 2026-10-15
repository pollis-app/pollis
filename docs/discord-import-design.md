# Discord import with message history: research, design, and attack

Related: #1180 (Discord structure import), #1179 (Slack import; its PR lands the shared scaffolding, and #1180 builds on it in its own PR), #1181 (history keeper epic), #847 (invite links), #99 (pinned messages / Kpin), #825 (threads), #856 (account export), #720 (envelope retention), #872 (bots as members).

**Shared scaffolding** (archive format, keys and the `Kpin` wrap, the waiting state, seats, the redeem change, claims, redaction and erasure, the `import/` R2 family, migration 000036, DS endpoints, shared pollis-core commands, shared invariants, mobile parity, backward compatibility, docs and principle changes) is **defined in the shared import spec, `docs/import-history-design.md`**. This file covers only what is specific to Discord. "Shared §N" points there.

Discord API text below was read from the official docs source, `github.com/discord/discord-api-docs` at commit `3714862` (2026-10-09), which renders at `docs.discord.com/developers/...`. Policy text is cited from Discord's support-dev site; where only secondary sources were reachable, the citation says so.

## Summary

- Full guild history is reachable with a bot the admin creates in their own Discord application, paging `GET /channels/{id}/messages` 100 at a time per channel and per thread. No gateway connection is needed for the crawl. Content comes back empty unless the app has the `MESSAGE_CONTENT` privileged intent, which is self-serve only while the app is visible to 10,000 or fewer unique users (changed 2026-06-10; the old "100 servers" rule in the brief and #1180 is obsolete).
- The brief's key handoff (send K over MLS as a new message type) is redundant: K is wrapped under the #99 pin key (shared §4). The "no K-holder online" case is not solved, only inherited: seat redeemers join by external commit and wait until a `Kpin` holder re-wraps (shared §4.2).
- Per-user DM delivery of claim links is the wrong channel. Discord's own Create DM docs say not to DM everyone in a server and threaten rate limits or blocks, and the Developer Policy names unsolicited DMs. v1 uses one announcement plus a user-initiated request (a reaction the admin device reads over REST, then a DM to that user only), with an optional gateway "claim desk" later (step 5). Either way Discord carries and stores the link, and the admin's device minted its secret.
- Pollis has no per-channel access control (all channels of a group share one MLS group). Importing a Discord private channel or private thread exposes it to every Pollis group member. The importer must default to public channels only and treat private ones as separate groups or skips.
- The brief's server-visible `source_identity_hash` with a per-group salt is brute-forceable (Discord ids are snowflakes, enumerable and publicly visible). The binding is an HMAC under a K-derived key (`source_ref`, shared §2).
- The biggest unresolved risk is policy, not crypto: Discord's Developer Terms require deleting stored API Data when access ends (9(b)), forbid sharing API Data with third parties unless a user "expressly directs" it (5(b)(iii)), and the people whose messages are imported but who never come to Pollis did not consent to the move. Read literally, 5(b)(iii) makes opt-in, not opt-out, the only Terms-compatible consent mode for sharing other people's messages with the new group. This design adds a consent window, per-author erasure (shared §8), and an author-corroboration path, but it needs a legal read before it ships.

## Scope change vs #1180

| | #1180 (today) | This proposal |
|---|---|---|
| Messages, threads, forum posts, attachments | Explicitly out | In, as an encrypted archive readable by current group members (shared §1, §3) |
| Members | Invite link + username mapping sheet | Per-identity seats (single-use #847 links) delivered on user request, plus signed claims (shared §5, §6) |
| DS awareness of Discord | None | Still none: the DS stores opaque archive blobs, seats, and claims. It never sees a Discord token, id, or name (shared I23) |
| Bot token | On device, in memory | Unchanged for the crawl. The optional claim desk additionally keeps a gateway session open while it runs |
| Source deletion | Out | A takedown tool, honest about what Discord allows |
| Principle change | None | Imported history is readable by joiners; supersedes the "never its messages" line of #1179/#1180 and the "no re-import" constraint on #856 (shared §3.5, §16) |

Adds on top of the shared scaffolding: the crawler, the consent window, the claim-request path, the takedown tool. Supersedes: #1180's "Explicitly not in scope: messages, attachments, threads, forum posts"; #1180's "member output is a shareable invite link plus a mapping sheet" (kept as the fallback); brief points 1 (reservation shape) and 3 (K handoff message type). #1180's PR lands after #1179's and adds no migration.

Keeps from #1180 unchanged: structure mapping, emoji collision policy, role collapse to member/admin with a report, dry run by default, idempotent re-run, page not modal, provider trait from #1179.

Correction to #1180 itself: "position preserved as ordering" has no target. `channels` has no position or category column (`pollis-schema/migrations/000000_baseline.sql:15`). Ordering needs a migration or is lost.

## Research findings

### Reading history

| Need | Endpoint / fact | Source |
|---|---|---|
| Messages | `GET /channels/{id}/messages`, `before`/`after`/`around` mutually exclusive, `limit` 1-100 (default 50), newest first. Requires `VIEW_CHANNEL`; without `READ_MESSAGE_HISTORY` it returns nothing (silently, not an error) | [Get Channel Messages](https://docs.discord.com/developers/resources/message#get-channel-messages) |
| Content | Without `MESSAGE_CONTENT`, `content`, `embeds`, `attachments`, `components` are empty and `poll` omitted, in REST responses too | [Message Object](https://docs.discord.com/developers/resources/message#message-object), [Message Content Intent](https://docs.discord.com/developers/events/gateway#message-content-intent) |
| Active threads | `GET /guilds/{id}/threads/active` (public and private) | [List Active Guild Threads](https://docs.discord.com/developers/resources/guild#list-active-guild-threads) |
| Archived public threads | `GET /channels/{id}/threads/archived/public`, paged by `archive_timestamp`; needs `READ_MESSAGE_HISTORY` | [List Public Archived Threads](https://docs.discord.com/developers/resources/channel#list-public-archived-threads) |
| Archived private threads | `GET /channels/{id}/threads/archived/private`; needs `READ_MESSAGE_HISTORY` and `MANAGE_THREADS` | [List Private Archived Threads](https://docs.discord.com/developers/resources/channel#list-private-archived-threads) |
| Forum / media posts | Forum (type 15) and media (type 16) channels hold only threads; posts use the thread endpoints. The starter message has the thread's id | [Threads: Forums](https://docs.discord.com/developers/topics/threads#forums) |
| Pins | `GET /channels/{id}/messages/pins`, `before` timestamp, `limit` 1-50; old `/pins` deprecated | [Get Channel Pins](https://docs.discord.com/developers/resources/message#get-channel-pins) |
| Reactions | Message objects carry emoji + count. Who reacted needs `GET .../reactions/{emoji}` per message per emoji, 100 users per page | [Get Reactions](https://docs.discord.com/developers/resources/message#get-reactions) |
| Replies, forwards | `message_reference`; forwards carry `message_snapshots`, immutable, at most 1 | [Message Object](https://docs.discord.com/developers/resources/message#message-object) |
| Edits | Only `edited_timestamp` and current content. The API has no edit history | [Message Object](https://docs.discord.com/developers/resources/message#message-object) |
| Counting before crawling | `GET /guilds/{id}/messages/search` (documented 2026-03-19): filters by author, channel, `has`, `pinned`; returns `total_results`; 202 + code 110000 while the index builds; `offset` max 9975; needs `MESSAGE_CONTENT` | [Search Guild Messages](https://docs.discord.com/developers/resources/message#search-guild-messages), [changelog](https://docs.discord.com/developers/change-log) |
| Structure without a bot crawl | Guild templates snapshot channels/roles/permissions (no messages); `POST /guilds/{id}/templates` needs `MANAGE_GUILD` | [Guild Template](https://docs.discord.com/developers/resources/guild-template) |

Attachments: CDN attachment URLs are signed with `ex` (expiry), `is` (issued), `hm` (signature) and expire; "when your app receives a payload with a signed URL (like when you fetch a message), it will be valid" ([Signed Attachment CDN URLs](https://docs.discord.com/developers/reference#signed-attachment-cdn-urls)). The documented refresh is re-fetching the message (`GET /channels/{id}/messages/{id}`). A `refresh-urls` endpoint exists in the client but is not in the official docs; do not build on it. Consequence: download each page's attachments in the same pass that fetched the page, and on a 403/404 from the CDN re-fetch that one message. Never store URLs for later.

Upload sizes on Discord: 10 MB free, 50 MB Nitro Basic, 500 MB Nitro (secondary sources only: [file.kiwi](https://file.kiwi/blog/discord-file-size-limit), [fast.io](https://www.fast.io/resources/discord-file-size-limit.md)). Pollis caps one object at `R2_MEDIA_MAX_BYTES` = 104 MiB (`pollis-api/src/broker.rs:99`), so Nitro-sized files exceed it and must be skipped and reported.

### Rate limits and crawl time

- Per-route limits are scoped per top-level resource (`channel_id`, `guild_id`), so different channels crawl in parallel; global cap is 50 requests/second per bot; bucket sizes are deliberately not published and must be read from `X-RateLimit-*` headers ([Rate Limits](https://docs.discord.com/developers/topics/rate-limits)).
- 10,000 invalid requests (401/403/429) per 10 minutes from one IP gets a temporary Cloudflare ban; `X-RateLimit-Scope: shared` 429s do not count ([Invalid Request Limit](https://docs.discord.com/developers/topics/rate-limits#invalid-request-limit-aka-cloudflare-bans)). The crawler must check channel permissions up front instead of collecting 403s, and stop on the first 401.

Messages are 100 per request. Effective rate R depends on the unpublished per-channel bucket and how many channels run in parallel (capped by 50/s). Text-only crawl time:

| Messages | Requests | R = 1/s | R = 5/s | R = 25/s | R = 50/s (ceiling) |
|---|---|---|---|---|---|
| 100k | 1,000 | 17 min | 3.3 min | 40 s | 20 s |
| 1M | 10,000 | 2.8 h | 33 min | 6.7 min | 3.3 min |
| 5M | 50,000 | 13.9 h | 2.8 h | 33 min | 17 min |

The table is a lower bound. Real cost drivers:
1. Attachments: CDN downloads plus Pollis re-upload. 5M messages with 5% carrying a 2 MB file is 500 GB. Bandwidth and Pollis R2 storage, not Discord rate limits, decide whether a big guild is feasible.
2. Reactor identities: one request per (message, emoji). 1M messages with 10% reacted at 2 emoji each is another 200k requests, 20 times the message crawl. Recommendation: import counts only (see mapping).
3. Thread enumeration: one listing per parent channel plus one crawl per thread. A forum with 20k posts is 20k small crawls, each its own bucket, so parallelism helps.

The dry run must measure R from headers on the first few hundred requests and show a projected time and storage size before anything is written.

### Privileged intents and app review

- Privileged intents: `GUILD_PRESENCES`, `GUILD_MEMBERS`, `MESSAGE_CONTENT` ([Privileged Intents](https://docs.discord.com/developers/events/gateway#privileged-intents)).
- As of 2026-06-10 the review threshold is unique users who can see the app across all its servers, not server count: "If that number exceeds 10,000, your app needs to apply for Privileged Intent access." Under 10,000 the intent is a portal toggle. Over it, a notice gives 90 days to apply, and the app keeps working while in review. Granted access must be re-applied for annually ([Getting Started with Privileged Intent Review](https://docs.discord.com/developers/gateway/getting-started-with-privileged-intent-review), announcement [support-dev 40281523410967](https://support-dev.discord.com/hc/articles/40281523410967)).
- So a single-guild, admin-created bot needs no approval only if the guild has 10,000 or fewer members who can see it. A larger guild triggers review, and the review asks for the use case and retention policy. "Migrate our history off Discord" may or may not be approved; assume it can be refused. Two details the docs add (verified in the repo clone at `3714862`, `developers/gateway/getting-started-with-privileged-intent-review.mdx:30,58,152`): over the threshold the portal gives 90 days to apply and the app "will continue to function with the intents you are requesting access to" while in review, so a crawl may finish inside that window; but the troubleshooting text also says the toggle is "locked" for an app already over the threshold, so whether a brand-new app joining a large guild can turn the intent on at all is ambiguous (unverified). The design must not rely on enabling the intent before inviting the bot to dodge the count. Also, the 2026-06-10 change split App Verification from intent review (`change-log.mdx:515`); App Verification is irrelevant to a one-guild bot.
- Wording nit: the review doc says "exceeds 10,000"; the changelog says "once your app reaches 10,000". Treat 10,000 as the boundary, not a safe value.
- `GUILD_MEMBERS` is required for `GET /guilds/{id}/members` ([List Guild Members](https://docs.discord.com/developers/resources/guild#list-guild-members)). Message authors arrive embedded in messages, so the crawl itself does not need it. Roles and the full member list for #1180 do.
- The crawl needs no gateway connection: `MESSAGE_CONTENT` gates REST fields by the portal setting. Older docs required a bot to identify on the gateway once before sending messages; current Create Message docs no longer say so. The dry run should verify with one test fetch that `content` is non-empty and stop with portal instructions if not.

### Identity

- Bots see `id`, `username`, `global_name`, `avatar`, `bot`, `public_flags`. `email` and `verified` need the user's own OAuth `email` scope ([User Object](https://docs.discord.com/developers/resources/user#user-object)). There is no path to members' emails, which confirms #1180.
- OAuth `identify` returns `/users/@me` without email ([OAuth2 scopes](https://docs.discord.com/developers/topics/oauth2#shared-resources-oauth2-scopes)). The implicit grant needs no client secret and returns the token in the URI fragment ([Implicit Grant](https://docs.discord.com/developers/topics/oauth2#implicit-grant)). That is usable from a member's Pollis client against the admin's app id with a registered redirect. It proves the id only to the member's own device; nobody else can verify it without a Discord-aware server. This design uses interactions instead (below).
- Create DM: "You should not use this endpoint to DM everyone in a server about something. DMs should generally be initiated by a user action. If you open a significant amount of DMs too quickly, your bot may be rate limited or blocked from opening new ones." ([Create DM](https://docs.discord.com/developers/resources/user#create-dm)). Errors: `40003` opening DMs too fast, `50007` cannot send messages to this user ([JSON error codes](https://docs.discord.com/developers/topics/opcodes-and-status-codes#json)).
- Users can turn off "Allow direct messages from server members" per server and as a default for new servers ([PCWorld](https://www.pcworld.com/article/615696/how-to-close-your-discord-direct-messages-and-keep-weirdos-out.html), [SlashGear](https://www.slashgear.com/1289550/how-block-someone-discord/); no official help article reached). (unverified: secondary sources only.) Discord publishes no figure for how many users do. The privacy-focused communities this targets are the ones most likely to have DMs closed, so plan for a large, unknown failure rate. Every `50007` means that seat gets no link.
- Interactions: a button click delivers `member` with the invoking user's id; the reply can be `EPHEMERAL` (flag 64), visible only to that user; the first response must arrive within 3 seconds, and the token lasts 15 minutes. Interactions arrive over the gateway or an HTTP endpoint URL ([Receiving and Responding](https://docs.discord.com/developers/interactions/receiving-and-responding)). Interaction endpoints are not bound by the global rate limit ([Rate Limits](https://docs.discord.com/developers/topics/rate-limits#global-rate-limit)).

### Deletion on Discord

- Single delete: `DELETE /channels/{id}/messages/{id}`; deleting another user's message needs `MANAGE_MESSAGES` ([Delete Message](https://docs.discord.com/developers/resources/message#delete-message)).
- Bulk delete: 2-100 ids, guild channels only, `MANAGE_MESSAGES`, "will not delete messages older than 2 weeks, and will fail with a 400 BAD REQUEST if any message provided is older than that" ([Bulk Delete Messages](https://docs.discord.com/developers/resources/message#bulk-delete-messages)); error `50034` ([codes](https://docs.discord.com/developers/topics/opcodes-and-status-codes#json)).
- No documented per-message delete rate. Community reports describe it as slow and throttled ([Discord feedback thread](https://support.discord.com/hc/en-us/community/posts/360040900231-Allow-bots-to-bulk-delete-messages-over-two-weeks-old)). At 1-5 deletes/second per channel, 1M old messages takes 2.3 to 11.6 days.
- Delete channel: `DELETE /channels/{id}` with `MANAGE_CHANNELS`, irreversible ([Delete/Close Channel](https://docs.discord.com/developers/resources/channel#deleteclose-channel)). For a full takedown this is the practical tool: one request per channel, versus days of single deletes. Current docs list no Delete Guild for apps, so deleting the server itself is something the owner does by hand in the Discord client.
- Account deletion: per secondary sources, messages remain, attributed to "Deleted User" ([Discord feedback](https://support.discord.com/hc/en-us/community/posts/19801310872599), [Simple Analytics review](https://www.simpleanalytics.com/is-gdpr-compliant/discord)). (unverified.) No official retention statement was reachable from this environment. Either way, "delete your Discord account" does not take down your messages.
- Discord's backend retention of deleted content is not documented. The takedown wording must be "removed from Discord's servers' view", never "erased".

### Exports and third-party tools

- Discord has no server-history export or migration feature. Server templates copy structure only ([Guild Template](https://docs.discord.com/developers/resources/guild-template)); a bot vendor states the same ([Xenon](https://xenon.bot/blog/export-discord-chat-history), secondary).
- "Request my data" package: per Discord's help text, "The messages folder contains all the messages that you have sent on Discord", grouped by channel id with `channel.json` metadata ([What is a Data Package?](https://support.discord.com/hc/en-us/articles/360004957991)). It holds only the requester's own messages, never anyone else's, so it cannot rebuild a channel. It is useful in another way: an author can use it to corroborate their own imported messages (see "Where this breaks").
- DiscordChatExporter accepts a bot or a user token. Its own README warns: "automating user accounts is against Discord TOS and may result in you getting banned" ([DiscordChatExporter](https://github.com/Tyrrrz/DiscordChatExporter)). Discord bans self-bots outright ([Automated user accounts (self-bots)](https://support.discord.com/hc/en-us/articles/115002192352-Automated-user-accounts-self-bots-)). Pollis must never accept a user token, and the import page must say why.

### Policy

Developer Policy, current numbered items as reported via search of the live page ([Developer Policy](https://support-dev.discord.com/hc/en-us/articles/8563934450327-Discord-Developer-Policy); full 2022 text verified at [discord-api-docs@48d3c2d](https://raw.githubusercontent.com/discord/discord-api-docs/48d3c2d324796d8236355262ace3b22c1d29acb0/docs/policies_and_agreements/Developer_Policy.md)):
- Purpose limitation: "Do not use API Data for any purpose outside of what is necessary to provide your stated functionality." The admin's app has migration as its stated function, so the reading is arguable either way.
- "Do not mine or scrape any data, content, or information available on or through Discord services." A permitted bot reading a server it was invited to is ordinary API use, not scraping, but the clause is undefined. Flag it; do not build on a favourable reading.
- "Do not use API Data in any way that goes against Discord users' expectations. This includes frequently sending unsolicited direct messages". This rules out per-member DM blasts.
- "Do not use message content obtained through the APIs to train machine learning or AI models" ([quoted in ojiverse/datawarehouse#22](https://github.com/ojiverse/datawarehouse/issues/22), secondary). Irrelevant to Pollis unless someone points an assistant at the archive (#1181 "Later").
- "Your Application must respect user-initiated blocks, bans ... visibility settings". Banned members' messages are still readable by a bot; whether to import them is a decision (default: import, since they are part of the record; flag in the report).

Developer Terms ([Developer Terms of Service](https://support-dev.discord.com/hc/en-us/articles/8562894815383-Discord-Developer-Terms-of-Service)). The wording below was checked against the full text at [discord-api-docs@48d3c2d `docs/policies_and_agreements/Developer_Terms_of_Service.md`](https://raw.githubusercontent.com/discord/discord-api-docs/48d3c2d324796d8236355262ace3b22c1d29acb0/docs/policies_and_agreements/Developer_Terms_of_Service.md) (effective 2022-10-01; lines 79, 81, 85, 141). The current repo (`developers/policies/developer-terms-of-service.mdx` at `3714862`) is only a stub that links the live page, so whether the live text still matches the 2022 text is (unverified):
- 5(b), sharing: "You will not share API Data with any third party, except ... (i) with a Service Provider; (ii) to the extent required under applicable laws or regulations; and (iii) when a user of your Application expressly directs you to share their API Data with the third party (and you will provide us proof thereof upon request)." The other members of the new Pollis group are third parties. Sharing author X's messages with them fits only (iii), and only for authors who expressly directed it. An opt-out window is not "expressly directs". Pollis, as the storage service the admin uses, is plausibly a "Service Provider" under (i), and 12(a) then requires a written agreement binding Pollis to use the data only for the admin; that is a contract Pollis would have to offer (unverified whether it applies).
- 5(b), retention: "promptly delete the API Data when: (a) retaining it is no longer necessary for your Application's stated ... functionality ...; (b) you stop operating your Application ...; (c) we request you delete it; (d) the applicable user requests you delete it". "You will give users an easily accessible way to ask for their API Data to be modified and deleted."
- 5(c): "commercially reasonable efforts ... These efforts will include: (i) encryption of the data at rest".
- 9(b): "Upon any termination of the Terms or discontinuation of your access to the APIs, you will ... delete any cached or stored API Data you have in your possession or control or was otherwise collected by your Application".

How these land: the admin who creates the application is the "developer" bound by these terms, not Pollis. A finished migration is exactly "you stop operating the app", and the archive in Pollis is stored API Data. Read literally, 9(b) and 5(b) conflict with keeping the archive after the bot is deleted. Arguments the other way: the data becomes members' own content held by members, and the people who claim seats direct their own data to move. Neither is settled. The sharing clause is the sharper problem: it bites at publish time, not at termination, and the "members' own content" argument only covers authors who opted in. Pollis should not tell admins this is compliant. The import page states the clause and links it, and legal review is a ship gate (Open decision 1).

### MCP

There is no official Discord MCP server. Community ones (e.g. [v-3/discordmcp](https://mcpservers.org/servers/v-3/discordmcp), [atrumcore/discordmcp](https://github.com/atrumcore/discordmcp)) wrap a bot token so an LLM can read and send channel messages. For this feature they are irrelevant at best: the importer is Rust in `pollis-core`, and routing guild plaintext through an LLM host contradicts the E2EE claim and risks Developer Policy item 21. At most a developer might use one to poke a test guild by hand. Nothing ships with it.

## Pollis facts this design is built on

| Fact | Where |
|---|---|
| Every channel of a group shares one MLS group; no per-channel ACL | `pollis-schema/migrations/000019_pinned_messages.sql` header; `channels` has no member table |
| Kpin: per-conversation random key, wrapped under `export_secret("pollis-pin-kek")`, re-wrapped at every epoch by any holder, CAS on `(generation, epoch)`; any current member including late joiners reads every pin | `.codesight/wiki/mls.md:622-660`, `pin_keystate` in 000019 |
| `max_past_epochs = 0` | `.codesight/wiki/mls.md:178` |
| Attachments are convergent (`key = HKDF(SHA-256(plaintext))`), liveness derived from `attachment_ref JOIN message_envelope` and `vault_attachment_ref JOIN vault_message` | `pollis-core/src/commands/r2.rs:706+`, `pollis-delivery/src/messages.rs:1913` |
| R2 key families are fixed server-side (`media/`, `emoji/`, `avatars/`, `group-icons/`) | `pollis-delivery/src/broker.rs:1428-1440` (`enum R2Family`) |
| Invite links: client-generated `selector.secret`, DS stores `sha256(secret)`, `CHECK (uses <= max_uses)`, redemption audit with `UNIQUE (link_id, user_id) WHERE succeeded = 1`, redemption adds `group_member` rows; the joiner's device external-joins | `000014_group_invite_link.sql`, `pollis-delivery/src/groups.rs:1587-1735`, `pollis-core/src/commands/groups/invites.rs:334-338,402` |
| A redeemer who is ALREADY a group member gets idempotent success with no use burned and NO redemption row written | `pollis-delivery/src/groups.rs:1687-1695` (`is_member` short-circuit before the transaction) |
| Account deletion deletes every link the user created and nulls `link_id` on other users' redemptions of them; group deletion does the same for the group's links | `pollis-delivery/src/teardown.rs:299-306`, `:511-519` |
| Invite URLs carry the token in the PATH: `https://pollis.com/invite/<token>`, `pollis://invite/<token>`; `extract()` drops any `?` or `#` suffix | `pollis-core/src/commands/groups/invite_token.rs:121-138` |
| Group teardown is `purge_group`; `purge_conversation_rows` is per channel/DM | `pollis-delivery/src/teardown.rs:444,488` |
| Reactions are a server-plaintext table keyed by Pollis `user_id` | `message_reaction` in baseline; `pollis-delivery/src/messages.rs:1449` |
| Replies (`reply_to_id`), threads (`thread_id` inside the MLS payload, #825), edits, redaction frames (0xF6), pins exist | `pollis-core/src/db/local_schema.sql:17`, `messages/framing.rs` |
| Production Turso runs `foreign_keys=OFF`; every new table must join the teardown lists or `tests/teardown.rs` fails | `pollis-delivery/src/teardown.rs` header |
| Guard-trigger precedent (`RAISE(ABORT)`) | `000017_conversation_guard_triggers.sql` |
| Local lookback eviction by `received_at` | `pollis-core/src/commands/messages/retention.rs` |
| #856 export says "No re-import ... An import path is exactly the backup channel the constraint on #856 forbids" | `pollis-core/src/commands/export.rs` header |

## End-to-end flow

### 0. Admin setup (once, about 5 minutes, documented on the page with screenshots)

The admin must create their own Discord application. Pollis cannot ship a shared bot: its token would be in every client binary, its user count would cross 10,000 at once (forcing review for every customer), and Pollis would become the "developer" holding every customer's API Data.

1. Developer Portal → New Application → name it (e.g. "<Guild> migration").
2. Bot tab → Reset Token → copy (shown once). Enable Message Content Intent. Enable Server Members Intent only if #1180 role/member mapping is wanted.
3. OAuth2 → URL Generator → scopes `bot` (and `applications.commands` for the claim button) → permissions: View Channels, Read Message History, Manage Threads (private archived threads; optional), Send Messages (announcement), Manage Messages + Manage Channels (takedown; can be granted later).
4. Open the URL, add the bot to the guild.
5. Paste the token into Pollis. It is held in memory by default; "remember on this device" stores it in the device keystore (`keystore.rs`), never the local DB and never the DS.
6. After the migration: Reset Token or delete the application. The page prompts for this at the end.

### 1. Plan (dry run, nothing written)

`import_discord_plan(token, guild_id)` in pollis-core:
- Structure per #1180.
- Channel classification by effective `@everyone` permissions: public (imported by default), private channel or private thread (excluded by default; options: separate Pollis group per private channel, or skip). See "Pollis has no per-channel ACL".
- Count estimate via Search Guild Messages `total_results` per channel (falls back to "unknown" on 202).
- Rate probe: measure R from headers on the first pages; content check (non-empty `content` on a known message).
- Output: projected messages, attachments, bytes, crawl time, R2 storage, privileged-intent threshold status (member count vs 10,000), and every lossy mapping.

### 2. Notice and consent window (Discord side, before any crawl)

The bot posts one announcement in a channel the admin picks:
- "This server is moving to Pollis. History from #a, #b, #c will be copied into an end-to-end encrypted archive that members of the new space can read. If you do not want your messages copied, react with the exclude emoji before <date>." Plus a claim reaction for seat links (step 5.2).
- "Exclude my messages" is a button interaction. The bot records the clicker's Discord id locally (on the admin device) in the exclusion list and answers ephemerally. Correction: a button click reaches the admin device only while its gateway session is open (step 5.3). With the desktop asleep, every click in a 14-day window fails with "This interaction failed", so as written the objection mechanism silently works only when the admin happens to be online. Either the consent button runs on the same desk with the window shown as "open only while the desk runs" (honest but weak), or consent uses a REST-readable signal that needs no gateway: the announcement carries two bot-seeded reactions ("include" / "exclude"), and at build and delta time the admin device reads reactors with `GET /channels/{id}/messages/{id}/reactions/{emoji}` (100 per page; one on-demand read, not a poll). The cost is that a reaction is public: other members see who opted in or out. State that in the announcement. **v1 uses the reactions;** the button exists only with the claim desk.
- Default window 14 days, admin-configurable, minimum 7. The crawl may run during the window; exclusions apply at archive build and at the delta pass. GDPR Art. 21 objection is not bounded by the window: an objection after publish must still be honoured through the per-author erasure tool (Consent and GDPR).
- Exclusion must cover an excluded author's content wherever it appears: their own records, `message_snapshots` of their messages forwarded by others, reply previews, and attachments. Mentions of them stay as a placeholder name. The doc previously filtered only by author (D3).
- Opt-in mode (Open decision 2): only authors who reacted "include" have content imported; everyone else becomes "[message from a member who did not opt in]" with timestamp only. Developer Terms 5(b)(iii) (Policy) points to opt-in as the default, not the option.

### 3. Crawl (admin desktop, resumable)

- REST only. Channels and threads crawled oldest-to-newest (`after=0`, then `after=last_id`), parallel up to a budget kept under 50 req/s minus headroom, with per-bucket accounting from `X-RateLimit-Bucket`.
- Each page: normalise to the archive record (below), download its attachments immediately from the fresh signed URLs, encrypt and stage locally.
- Checkpoint per channel: `(channel_id, last_message_id, done)` in the local SQLCipher DB, so a crash, sleep, or quit resumes. The token is not persisted unless the admin chose "remember", so resuming after a restart asks for it again.
- Stop conditions: 401 → stop all; 403 on a channel → mark it unreadable, report, never retry; 429 → obey `retry_after`; CDN 403/404 → re-fetch that message once, then record the attachment as lost.

### 4. Build, encrypt, upload, publish (Pollis side)

The pipeline is shared (`import_apply`, shared §3, §4.1): ensure `Kpin`, mint `K`, chunk, encrypt and upload attachments under random keys (downloaded in the same pass that fetched each page, step 3), sign and publish the write-once manifest. Discord specifics:

- `src_id` is the Discord message id. `source_ref` uses provider `"discord"` and an empty namespace (shared §2).
- The manifest's `consent` records the mode (Open decision 2) and `exclusion_list_hash`; excluded authors appear in `identities` with `kind = "excluded"`, and their content is filtered before encryption (D3).
- Delta pass at cutover, **before publish**: re-crawl `after=last_id` for new messages, and optionally re-list every message id to drop messages deleted on Discord since the first pass. It is what makes "your Discord deletions up to cutover are respected" true. The manifest is write-once (shared §3.3), so anything found after publish becomes a new import.
- Re-running with the same local job id is idempotent: chunk ids already acknowledged are skipped, and the DS refuses duplicates.

### 5. Seats and claims

Seat shape, lazy minting, `seat_sig`, the redeem change (including the existing-member branch and `SeatGrant`), claim signing and verification, withdraw and revoke are **defined in the shared import spec** (shared §5, §6). Discord specifics:

1. One seat per distinct non-bot author, created right after publish without a link (lazy mint, shared §5.1), so no live link secret exists for a seat nobody asked for.
2. **v1 request path (no gateway, no CLAUDE.md exception):** the announcement (step 2) asks members to react with the "claim" emoji. On demand (an admin action, never a timer) the admin device lists reactors with `GET /channels/{id}/messages/{id}/reactions/{emoji}` (100 per page), mints a link for each reactor's seat (`mint_import_seat_links`, short expiry), and DMs it with `SUPPRESS_EMBEDS`. Each DM is then initiated by a user action, which is what Create DM's guidance asks for; `50007` closed-DM failures remain and are reported. The link form is shared §5.2: `pollis://invite/<token>` plus the bare token.
3. **Optional claim desk (later):** while the admin's desktop runs the "Claim desk" page it keeps a gateway session; a "Claim my Pollis seat" button press carries the clicker's Discord id, and the device defers, mints, and replies `EPHEMERAL` (64) with `SUPPRESS_EMBEDS` (4) inside the 15-minute token. It proves to the admin's device, and nobody else, that the Discord account with that id asked. Discord relays and stores the reply; the admin minted the secret and can redeem it first (Abuse: sockpuppet). The heartbeat needs a written CLAUDE.md exception (Open decision 3).
4. Fallbacks: the admin hands links out by hand, or bulk-DMs at a deliberately slow rate after an explicit warning, accepting `50007`/`40003`.
5. Rendering: claimed authors show "Imported from Discord" with the original display name on hover; unclaimed show "<global_name> (Discord)" (shared §7).

### 6. Member actions on their imported identity

Withdraw, delete my imported messages (claimant rewrite with tombstones), and dispute ("report misattribution") are shared (shared §6, §8). Discord adds:

- Corroborate: optionally load your own Discord data package; messages whose Discord id and content hash match get a "confirmed by author" mark, a `corroborate` annotation signed by the claimant (shared §8, §10). This is the only content-authenticity signal available.

### 7. Takedown on Discord

Page: Import → Takedown. Options, in order of practicality:

| Action | Mechanism | Cost |
|---|---|---|
| Remove imported channels | `DELETE /channels/{id}` | 1 request per channel; irreversible; needs `MANAGE_CHANNELS` |
| Remove a claimant's own messages on request | Ids from the local crawl index (or Search Guild Messages `author_id`); bulk delete for under 14 days, single delete otherwise | days for heavy posters; per-message throttled |
| Remove everything message by message | bulk + single deletes | impractical above about 100k old messages (2-12 days per 1M) |
| Delete the server | Owner, by hand, in the Discord client | out of Pollis's hands |

UI wording: "Your community's history now lives in an encrypted Pollis archive. You can take it down from Discord. Discord does not tell us whether it keeps deleted content internally, so treat this as removal from Discord's view, not proof of erasure." Never "erased".

### Does on-device-only survive a multi-day crawl?

Yes for the crawl: it is REST bursts with checkpoints, there is nothing for the DS to do, and a sleeping laptop just resumes. The cost is that the admin's desktop must be awake for the crawl to make progress; that is acceptable and should be stated with the projected time. The claim desk is different: it needs a gateway session that heartbeats, which is a periodic keepalive. CLAUDE.md bans `setInterval` keepalives. The gateway heartbeat is a protocol requirement of a foreign service, scoped to an explicit page the admin keeps open, but it still needs a written exception in CLAUDE.md alongside the code, or the claim desk is dropped in favour of the manual fallbacks. The codebase has no precedent to lean on: it explicitly avoids heartbeats elsewhere (`pollis-core/src/realtime.rs:150`, `commands/autolock.rs:17`, `commands/mls/self_update.rs:63`). The "token on device, DS gains no Discord awareness" claim does hold for the desk (no server is involved), but only because the admin's desktop plays the server, which is why it must stay awake. The reaction-plus-DM path in step 5.2 needs neither a gateway nor an exception, and should be v1; the desk is an optional extra.

## Pollis changes

Migration `000036_import_archive.sql`, DS endpoints, the redeem change, the R2 family, teardown, the shared commands and local tables, backward compatibility and the docs list are **defined in the shared import spec** (shared §3-§16) and land in #1179's PR. #1180's PR adds no migration and no DS endpoint. This section lists only what Discord adds.

### pollis-core (`pollis-core/src/commands/import/discord/`)

- `client.rs` (REST, bucket-aware limiter from `X-RateLimit-*`, no gateway), `crawl.rs` (checkpointed per channel in `import_job`), `normalize.rs` (Discord message to the shared `Record`, refusing #856 export input, shared I24), `consent.rs` (announcement, reaction reads, exclusion list), `takedown.rs`, and optional `claim_desk.rs` (gateway session, interactions only, `GUILDS` intent).
- Commands (Discord-only; everything else is shared §13): `import_discord_plan(token, guild_id) -> ImportPlan`, `discord_post_announcement(plan_id, channel_id)`, `discord_read_consent(plan_id)`, `discord_send_seat_links(import_id)` (reaction-then-DM path), `discord_takedown(import_id, action)`, and later `discord_claim_desk_start` / `_stop`.
- The bot token type is not `Serialize`; it lives in memory or, on "remember on this device", in the keystore, never in the local DB or any DS body (D2).

### Mapping into the shared record (shared §3.1)

| Discord | Pollis | Notes |
|---|---|---|
| Text / announcement channel (public) | channel + its imported history | |
| Private channel | excluded by default; option: its own Pollis group | no per-channel ACL |
| Category | name prefix or skip (#1180 decision) | no column |
| Public / announcement thread | `thread_root` in the parent channel (#825 semantics) | |
| Private thread | excluded by default | same ACL problem |
| Forum / media post | thread root + replies; tags as a text prefix | |
| Voice-channel text chat | the voice channel's text if Pollis has one, else skip with report | |
| Reply | `reply_to`; a dangling reply renders "original not imported" | |
| Forward | `kind = forward`, `ext.snapshot` from `message_snapshots` | excluded authors' snapshots dropped (D3) |
| Reactions | `reactions` with emoji + count, no `authors` | reactor lists cost a request per (message, emoji) and expose who reacted (Open decision 7) |
| Edits | `edited_at`; current text only | no history in the API |
| Pins | `pinned` flag; "Imported pins" (shared §7) | not real Pollis pins, so the DS learns nothing |
| Custom emoji in text | rewritten to the group emoji imported by #1180, else `:name:` text | |
| Stickers | `ext.stickers`, rendered `[sticker: name]` | |
| Polls | `ext.poll`: static question, answers, final counts | |
| Embeds | URL + title + description text; no remote image fetch | privacy |
| Mentions `<@id>` | `<src:SOURCE_REF>` | |
| System messages | dropped, except thread-created and pin notices, which become `kind = system` lines | |
| Bot / webhook authors | `kind = bot`, rendered "(bot)", unclaimable | |
| Excluded authors | timestamp-only placeholder | consent |

### UI

Desktop (`frontend/`): page `/settings/import/discord`, sharing the import shell with #1179's Slack page. Steps: Connect, Plan, Notice window (announcement status, exclusion count), Crawl (progress, ETA, pause), Review report, Publish, Seat links (reaction-then-DM; claim desk if shipped), Takedown. Register the page in `router.tsx`, `PAGE_RESULTS` in `SearchPanel.tsx`, and the Sidebar nav. In-channel rendering, claim landing, profile "Imported identities" and the per-message bar actions are shared (shared §7, §15) with provider text "Discord".

Mobile (`mobile/`): everything a member does is shared and ships in #1179's PR; #1180 adds corroborate-with-data-package (file picker) and a read-only takedown status view. Maestro `import-discord-corroborate.yaml`; Playwright `e2e/import-discord.spec.ts` (plan and apply against a mock Discord REST server).

Mobile exemptions, stated in #1180's PR: the admin import job (crawl), the claim desk and the takedown actions. Reasons: (1) iOS gives no guarantee of long-running background execution ([BackgroundTasks](https://developer.apple.com/documentation/backgroundtasks)), so a multi-hour crawl or a gateway session dies when the app is backgrounded; (2) a bot-token paste and Developer Portal setup is a desktop task; (3) the takedown's destructive Discord calls belong with the token holder. Short admin seat actions are not exempt (shared §15).

### Docs (Discord-specific, beyond shared §16)

The CLAUDE.md gateway-heartbeat exception, only if the claim desk ships (Open decision 3); the consent-mode and Developer Terms text on the import page; the takedown wording.
## Invariants and tests

Shared invariants I1-I28 are **defined in the shared import spec** (shared §14): seat link shape, one seat per source and per user, existing-member redeem-and-claim, claim needs the seat's redeemer, immutable claims and no re-claim, client claim verification including `claim_proof`, survival of the minter's account deletion, chunk version CAS, rewrite acceptance, R2 gates, teardown, imports never in `message`, random `K`, no source identity in DS bodies, #856 refusal. Discord adds:

| # | Invalid state | Enforced by | Test (must fail with the guard removed) |
|---|---|---|---|
| D1 | Private-channel or private-thread history imported into a group by default | planner default + an explicit per-channel confirmation in the plan | flows test `private_channel_excluded_by_default` |
| D2 | Bot token reaches the DS or the local DB | token type is not `Serialize`; keystore only | `discord_token_never_serialized` |
| D3 | An excluded author's content in a chunk, including as a forward snapshot or reply preview inside someone else's record | build step filters before encryption; the manifest records the exclusion-list hash | `excluded_author_has_no_body`, `excluded_author_not_in_forward_snapshot` |
| D4 | A user token accepted as the crawl credential (self-bot) | `client.rs` sends `Authorization: Bot`; the token check refuses anything else | `user_token_is_refused` |
| D5 | Stored CDN URLs reused after their signature window | attachments downloaded in the page's own pass; no URL column in `import_job` | `crawl_never_persists_cdn_urls` |

Integration: `flows::imports` (shared) plus `flows::imports_discord` with a mock Discord REST server, never the real API.
## Where this breaks

### Abuse

| Case | What happens | Mitigation | Residual |
|---|---|---|---|
| Admin fabricates messages, then a real person claims the identity | The claimant's Pollis name appears on invented text | Claim text says "this Discord account was me", never "I wrote all of this"; permanent "imported, not verified" badge; importer signature names who built the archive; author corroboration via own data package; claimant can delete records | Unclaimed and uncorroborated content is exactly as trustworthy as the admin. Say so in the UI |
| Admin claims an identity with a sockpuppet | The sockpuppet redeems the seat link (the admin holds every secret) and claims "@alice" | The claim shows the Pollis account that made it; the Discord-side binding comes only from the claim desk's interaction, run by the same admin | Not preventable without a Discord-aware third party. The admin is trusted for identity mapping just as for content. Document it |
| Leaked seat link | First redeemer gets the seat and can claim | Links are single-use, expire, are minted only on request and DM'd to the requesting user; admin can revoke the seat (shared §6); the real person sees "already claimed" and files a dispute | A link forwarded by its owner is the owner's choice |
| Someone claims a Discord identity that is not theirs without a link | Blocked by an honest DS: the trigger requires the seat's `redeemed_by` | Against a compromised DS (untrusted per CLAUDE.md), clients check `claim_proof` against the admin-signed `proof_hash` (`seat_sig`, shared §5.1, I9) | |
| Hostile member exfiltrates K | They can decrypt the archive from R2 indefinitely, including after removal, if they also get DS/R2 access | Same exposure as Kpin today (`mls.md` pin section); the archive is static, so a removed member gains nothing they could not have cached | Future imports wrapped under the same Kpin are also exposed. Rotating Kpin on removal is the fix for both and costs O(pins + imports) re-wraps; not in v1 (shared §4.3) |
| Claimant abuses rewrite to delete others' records | Detected: leaf verification fails | Clients keep the previous version on verification failure and report it; every tombstone is signed by its redactor (shared §8, I17) | Availability: a malicious rewrite can force clients onto the previous version. Do NOT keep a prior version server-side (an earlier draft proposed it): that would retain exactly the records a claimant or the per-author erasure tool removed. Clients keep their own last-good cached copy instead, and an admin re-publishes the chunk |
| Admin imports a Pollis export disguised as Discord | Becomes a backup/restore path #856 forbade | It needs a modified client and only re-publishes what the admin already reads | This is a principle change. State it rather than pretend the importer prevents it (shared §3.5, §16) |

### Consent and GDPR (serious)

- People whose messages are imported but who never move to Pollis are data subjects with no account and no say. A community migration is not "purely personal or household activity" once content is available to a large group; the CJEU held that publishing to an indefinite number of people falls outside that exemption ([Lindqvist C-101/01](https://curia.europa.eu/juris/liste.jsf?num=C-101/01); [GDPR Art. 2(2)(c)](https://eur-lex.europa.eu/eli/reg/2016/679/oj)). The admin or community is then a controller. Lawful basis would be legitimate interest ([Art. 6(1)(f)](https://eur-lex.europa.eu/eli/reg/2016/679/oj)), which needs a balancing test; data not obtained from the subject triggers information duties ([Art. 14](https://eur-lex.europa.eu/eli/reg/2016/679/oj)); they keep erasure and objection rights ([Arts. 17, 21](https://eur-lex.europa.eu/eli/reg/2016/679/oj)).
- What the design does about it: the in-guild notice and opt-out window (Art. 14 information and Art. 21 objection before the copy); optional opt-in mode; a per-author erasure tool for admins (`erase_import_source`, shared §8), which rewrites chunks; the claim-then-delete path for authors who do come; a delta re-check at cutover so Discord deletions are respected; Discord names stored only inside the encrypted manifest.
- What it does not fix: a person who never reads the announcement during the window. Opt-out notices reach only the people who see them. For communities with EU members, opt-in mode should be the default (Open decision 2).
- Pollis's position: Pollis stores ciphertext it cannot decrypt and holds no key, but encrypted personal data is still personal data. Pollis cannot act on an erasure request directly. It can only route the request to the group's admins, and it can delete a whole import on court order. The retention policy must say so.
- Discord Developer Terms 5(b)/9(b) (deletion on stop or termination) bind the admin and conflict with keeping the archive. Unresolved. Legal review is a ship gate.

Review assessment of this section (what is inaccurate, what is theater):
- Household exemption: Lindqvist is about publishing on an internet page open to an indefinite number of people. A Pollis group is closed, so Lindqvist is not directly on point for a 30-person friend server. The better-fitting line is that community organisations that collect members' data are controllers (CJEU Jehovan todistajat, C-25/17), and large public servers resemble publication. The conclusion (admins are usually controllers) stands; the citation overstates how settled it is for small groups.
- Art. 9 is missing and it is the hard part. Years of chat history routinely contain health, sexuality, religion, and political opinion. Legitimate interest (Art. 6(1)(f)) is no basis for special-category data; Art. 9(2) needs explicit consent or another listed ground, and "manifestly made public" (9(2)(e)) is weak for a members-only server. Opt-out cannot cover that content. This, plus Developer Terms 5(b)(iii), makes opt-in the defensible default, not "recommended for EU-heavy communities".
- Art. 14 is not met by the announcement as drafted. Art. 14(1)-(2) require the controller's identity and contact, purposes, legal basis, recipients, retention period, and the rights to erasure, objection, and complaint to a supervisory authority; 14(3)(a) sets at most one month. The Art. 14(5)(b) disproportionate-effort exemption still requires "appropriate measures ... including making the information publicly available". A pinned in-guild notice with the full Art. 14 content is plausibly that measure; the one-line draft text is not.
- Art. 21 is not a window. Objection runs at any time; the 14-day framing must not imply a deadline. The per-author erasure tool is therefore load-bearing, and it depends on an admin device being alive.
- Art. 17 reach: erasure rewrites the server chunk, but members' devices hold decrypted chunks in `import_chunk_cache`, and removed members keep K. Erasure must bump chunk versions so every client evicts its cached copy on next sync, and the UI must say that copies already on members' devices are outside Pollis's reach (as with Discord itself).
- Theater as drafted: (a) the "Exclude my messages" button, which fails whenever the admin's desktop is not running the gateway desk (step 2); (b) "exclusion count" in the manifest, which tells members nothing actionable; (c) "Discord names only inside the encrypted manifest", which is good hygiene but irrelevant to the data subject, since every member can read it. Not theater: per-author erasure, opt-in mode, the delta re-check, and excluding private channels by default.
- Pollis's own position: "encrypted personal data is still personal data" is stated as settled. After CJEU EDPS v SRB (C-413/23 P, 2025), whether pseudonymised or encrypted data is personal data for a recipient without the means to re-identify is assessed from that recipient's position (case and holding (unverified) in this environment). Pollis should not rely on either reading. It should offer the admin a processor agreement (Art. 28), and doing so also covers the Developer Terms 12(a) Service Provider requirement.
- Minors: Discord's minimum age is 13 (unverified); a migrated archive will contain minors' data, which raises the balancing-test bar under Art. 6(1)(f).

### Operational failure

| Failure | Behaviour |
|---|---|
| Crawl dies halfway | Per-channel checkpoints in the local DB; resume from `last_message_id`; staged ciphertext reused; the DS import stays `staging` and invisible |
| Laptop sleeps for days | Resume; the delta pass at cutover catches up. Signed URLs in staged pages are not reused; attachments were fetched in the same pass |
| Rate limits tighten mid-crawl | The limiter follows headers; the ETA updates; no hard-coded rates |
| Token reset or app deleted mid-crawl | 401 → stop; resume with a new token |
| Admin's device lost before publish | The `staging` import is orphaned. An admin can delete it; the existing DS sweep drops `staging` imports older than 30 days (shared §10). That is the only TTL, and it applies to unpublished uploads, never to messages |
| Admin's device lost after publish | Nothing lost: K is under Kpin, held by every member |
| CDN URL expiry | Re-fetch the single message once; else record the attachment as lost in the report |
| 5M-message guild | The text crawl is hours; attachments dominate (hundreds of GB). Needs a per-group storage quota (business tier), attachment filters (by size, type, age), and an "attachments: skip / metadata only" option. Members fetch chunks lazily; full-archive search on a phone is not offered; desktop offers "download all for search" |
| Over 10,000 visible users | Privileged intent review; the import may be refused or delayed. Per the review doc the app keeps functioning with the requested intents during a 90-day apply window, but whether a new app can enable the intent once over the threshold is ambiguous (unverified). The plan step detects the count and says so before setup |
| Interaction 3-second deadline | The claim desk defers, then follows up (15-minute token); seat lookup is local |
| Claim desk offline | The button fails; the announcement says to retry; manual fallbacks |
| Two admins import the same guild | Two independent imports; duplicate history. The planner warns when a published import with the same guild-id hash (inside the manifest, so client-side only) exists |

### Things in the brief or tickets that are wrong

1. "`group_reservation(..., role, channel_grants, display_hint, source_identity_hash ...)`". `channel_grants` has nothing to grant (no per-channel ACL). `role` pre-assigning admin makes a leaked link an admin takeover; seats grant member only. `display_hint` in clear leaks Discord identities to the operator; it belongs in the encrypted manifest. A per-group salted hash with a server-visible salt is brute-forceable over snowflakes; use an HMAC under a K-derived key. A separate reservation table is also unnecessary for admission: #847 links with `max_uses = 1` already are single-use seats, so only the binding table is new.
2. "Any device that holds K sends K to them as an ordinary MLS application message after the Add". Redundant and worse: it is N senders times M joiners of duplicate traffic, plus a new frame type old clients do not know. #99 already solved it: wrap K under Kpin.
3. "Uploads ciphertext as a blob (attachment-style object store)". Attachment storage is convergent with liveness tied to message envelopes; the archive must be random-keyed with its own liveness rows and its own R2 family.
4. "DM'd via a source-platform bot". Discord's Create DM docs and Developer Policy argue against it, and closed DMs make it fail silently at unknown scale. Use a user-initiated request (reaction then DM, step 5.2), or later the claim desk.
5. "Optional OAuth binding at redemption (verify the redeemer's Discord id matches)". Done on the redeemer's own device, it verifies nothing to anyone else, and a modified client skips it. Verification has to happen at the admin's side; the interaction gives that without OAuth.
6. "Privileged intents and the verification requirement for bots in 100+ guilds" (also in #1180). Since 2026-06-10 the threshold is 10,000 unique visible users, with annual re-review.
7. "Pre-join loss ... This must be stated explicitly as a principle change". Correct, but it is not the first: pins already let joiners read pre-join content, and §6.8 does not mention it. Fix the existing doc gap in the same PR.
8. "Removed members keep K (they already have the plaintext)". Only true if they downloaded the chunks; with lazy fetch they may not have, yet with K and a breach they can read all of it. Exposure is "everything in the archive", not "what they saw".
9. #1180: "position preserved as ordering". No column exists.
10. #1180/#1179: "this is a burst of reads, not a poll loop, so it does not conflict with the no-periodic-polling rule". True for the crawl. False for any gateway use (claim desk), which heartbeats.
11. #1180: "Explicitly not in scope: ... threads, forum posts". Superseded; and private threads need `MANAGE_THREADS` to list once archived.

## Open decisions

1. Legal review of Discord Developer Terms 5(b)/9(b) and Policy "mine or scrape" against this use, and GDPR controller guidance for admins. Ship gate. **Still open.**
2. Consent default: opt-out window (practical) or opt-in (strict; loses most content in big guilds). Original proposal: opt-out by default. Review: opt-in by default (Developer Terms 5(b)(iii) "expressly directs", GDPR Art. 9), with opt-out available only as an admin override that the page flags as non-compliant with the Terms as read; the mode is recorded in the manifest's `consent` (shared §3.3) and shown to members. **Still open (owner).**
3. Claim desk: accept the gateway heartbeat exception to CLAUDE.md, or ship with the reaction-then-DM path only. **Resolved for v1:** reaction-then-DM (step 5.2), no gateway, no exception; the desk is a later addition with its own exception.
4. Kpin rotation on member removal. **Resolved for v1 (shared §4.3):** not rotated, documented residual; the wrap is CAS-updatable for a #99 follow-up.
5. Private channels: skip, or one Pollis group per private channel.
6. Categories: name prefix now, or ship channel grouping (#1180's open question) first.
7. Reactor identities: counts only (proposed) or full reactor lists for claimed members (200k+ extra requests per 1M messages, and it exposes who reacted to what). The shared record supports both (`reactions.authors` optional).
8. Storage quota and tier for archives and attachments. **Shared (shared §18).**
9. Whether claimants may hold more than one live source identity per import (alt accounts). **Resolved for v1 (shared §6, I3):** no; override is shared open decision 4.
10. Slack (#1179) reuse and ticket structure. **Resolved (shared §0):** the shared scaffolding lands once, in #1179's PR (Slack is the first provider); #1180 builds on it in its own PR with no migration; both tickets' "never its messages" text is amended before code.

## Sources

Discord API docs (source `github.com/discord/discord-api-docs` @ `3714862`, 2026-10-09):
- https://docs.discord.com/developers/resources/message (Message Object, Get Channel Messages, Search Guild Messages, Delete Message, Bulk Delete Messages, Get Reactions, Get Channel Pins)
- https://docs.discord.com/developers/resources/channel (archived thread lists, Delete/Close Channel)
- https://docs.discord.com/developers/resources/guild (List Active Guild Threads, List Guild Members)
- https://docs.discord.com/developers/resources/guild-template
- https://docs.discord.com/developers/resources/user (User Object, Create DM)
- https://docs.discord.com/developers/topics/threads
- https://docs.discord.com/developers/topics/rate-limits
- https://docs.discord.com/developers/topics/oauth2
- https://docs.discord.com/developers/topics/opcodes-and-status-codes
- https://docs.discord.com/developers/reference#signed-attachment-cdn-urls
- https://docs.discord.com/developers/events/gateway#privileged-intents
- https://docs.discord.com/developers/gateway/getting-started-with-privileged-intent-review
- https://docs.discord.com/developers/interactions/receiving-and-responding
- https://docs.discord.com/developers/change-log
- https://support-dev.discord.com/hc/articles/40281523410967

Discord policy and help:
- https://support-dev.discord.com/hc/en-us/articles/8563934450327-Discord-Developer-Policy
- https://raw.githubusercontent.com/discord/discord-api-docs/48d3c2d324796d8236355262ace3b22c1d29acb0/docs/policies_and_agreements/Developer_Policy.md
- https://support-dev.discord.com/hc/en-us/articles/8562894815383-Discord-Developer-Terms-of-Service
- https://support.discord.com/hc/en-us/articles/115002192352-Automated-user-accounts-self-bots-
- https://support.discord.com/hc/en-us/articles/360004957991
- https://support.discord.com/hc/en-us/community/posts/360040900231-Allow-bots-to-bulk-delete-messages-over-two-weeks-old
- https://support.discord.com/hc/en-us/community/posts/19801310872599

Secondary:
- https://github.com/ojiverse/datawarehouse/issues/22 (quotes of Developer Terms 5(b), 5(c), 9(b) and Policy item 21)
- https://github.com/Tyrrrz/DiscordChatExporter
- https://xenon.bot/blog/export-discord-chat-history
- https://file.kiwi/blog/discord-file-size-limit
- https://www.fast.io/resources/discord-file-size-limit.md
- https://www.pcworld.com/article/615696/how-to-close-your-discord-direct-messages-and-keep-weirdos-out.html
- https://www.slashgear.com/1289550/how-block-someone-discord/
- https://www.simpleanalytics.com/is-gdpr-compliant/discord
- https://mcpservers.org/servers/v-3/discordmcp
- https://github.com/atrumcore/discordmcp

Law and platform:
- https://eur-lex.europa.eu/eli/reg/2016/679/oj (GDPR Arts. 2, 6, 14, 17, 21)
- https://curia.europa.eu/juris/liste.jsf?num=C-101/01 (Lindqvist)
- https://developer.apple.com/documentation/backgroundtasks

Pollis code: `pollis-schema/migrations/000000_baseline.sql`, `000012_attachment_ref.sql`, `000014_group_invite_link.sql`, `000017_conversation_guard_triggers.sql`, `000019_pinned_messages.sql`, `000031_attachment_ownership.sql`; `pollis-delivery/src/groups.rs`, `messages.rs`, `broker.rs`, `teardown.rs`; `pollis-api/src/lib.rs`, `broker.rs`; `pollis-core/src/commands/r2.rs`, `export.rs`, `messages/framing.rs`, `messages/retention.rs`, `db/local_schema.sql`; `.codesight/wiki/mls.md`; `docs/security-whitepaper.md` §6.8, §13; `docs/backend-core-invariants.md`.

## Review notes

Adversarial review, 2026-10-15, against repo `ec22dec` and `discord-api-docs@3714862`. Each line is one change made above or one open concern. Shared items were then carried into `docs/import-history-design.md` and are marked **Resolved (shared §N)**; the schema names and invariant numbers (I16-I21) in "Changes made" refer to this file's earlier draft and are superseded by shared §10 and §14.

### Changes made

- Summary: "no K-holder online is solved" was false. Seat redeemers external-join (`invites.rs:334-338,402`), so Kpin arrives only after a holder re-wraps (`mls.md:649-652`). Reworded as inherited, not solved.
- Summary and step 5.2: "only that user sees the link" omitted Discord (relays and stores the ephemeral reply) and the admin (minted the secret). Corrected, with what the interaction does and does not prove.
- Step 5.2: link shape `pollis://join/<selector>.<secret>` does not exist; real forms are `https://pollis.com/invite/<token>` and `pollis://invite/<token>` (`invite_token.rs:117-138`), with the secret in the path. Fixed, and added `SUPPRESS_EMBEDS`.
- Step 5.4: an existing member redeeming a seat link gets idempotent success with no redemption row (`groups.rs:1687-1695`), so the old claim trigger could never pass for them. Added the seat branch in redeem and invariant I18. **Resolved (shared §5.3, I6).**
- Step 5.4: the member's client had no way to learn its `source_ref`. Added `SeatGrant` in the redeem response. **Resolved (shared §5.3).**
- Step 5.5: the claim lacked `identity_version`; account keys rotate (`account_key_log`, 000005). Added.
- Step 5.5: the seat binding was enforced only by a DS trigger, but the DS is untrusted. Added `claim_proof`/`proof_hash` so clients can verify it (I19). **Resolved (shared §5.1, §6, I9),** with `seat_sig` added because lazy minting keeps `proof_hash` out of the manifest.
- Schema: the claim trigger joined `group_invite_link_redemption`, which teardown mutates (`teardown.rs:299-306,511-519`). Moved the redeemer onto the seat row (I20). **Resolved (shared §5.5, I10).**
- Schema: after withdrawal the same user could re-claim with the old redemption (the partial unique index covers only live rows), contradicting step 6. Claims are now keyed by `seat_id` (I17). **Resolved (shared §6, I8):** no re-claim by that user in v1.
- Schema: the seat trigger allowed a used, revoked, or non-expiring link. Now requires `uses = 0`, `revoked_at IS NULL`, `expires_at IS NOT NULL` (I21). **Resolved (shared §10, I1).**
- Schema: signature `BETWEEN 2000 AND 2600` loosened a fixed size; ML-DSA-44 is 2420 bytes (`account_identity.rs:9`, `ds_client.rs:17`). Now `= 2420`.
- Schema: the claim trigger did not require current membership. Now joins `group_member`.
- Schema: listed what the schema still admits (orphan chunk/blob inserts, claims surviving removal, unvalidated refs). **Resolved (shared §10):** insert gates on chunks and objects; the remaining admitted states are listed there with their enforcing layer.
- Teardown: `purge_conversation_rows` is per channel/DM; group delete is `purge_group` (`teardown.rs:444,488`). "Four tables" was five. Fixed. **Resolved (shared §11),** now seven tables.
- Facts table: R2 families are four, not three (`group-icons/` too, `broker.rs:1428-1440`). Added the redeem, teardown, and URL facts the design depends on.
- Abuse table: "keep one prior version server-side" would retain erased records. Replaced with client-side last-good caching.
- Step 2: the "Exclude" button needs the gateway desk online for the whole window. Added the REST-readable reaction alternative, a no-deadline Art. 21 note, and exclusion of forwarded snapshots and reply previews (I16).
- Privileged intents: verified the 10,000 threshold, the annual reapplication, and the 90-day window in the repo clone; added the ambiguity about "locked" toggles and the split between App Verification and intent review. The 100-server rule is obsolete for intents only.
- Developer Terms: verified 5(b), 5(c), and 9(b) wording against the 2022 full text at `48d3c2d`; the current repo copy is a stub, so whether the live text is current is (unverified). Added the 5(b)(iii) sharing clause, which the draft missed and which argues for opt-in. Added 12(a) Service Provider.
- Developer Policy quotes (purpose limitation, "mine or scrape", unsolicited DMs, blocks/visibility) verified verbatim in `48d3c2d` `Developer_Policy.md:25,26,33,52`.
- Discord API claims verified in the clone: Create DM guidance (`user.mdx:332`), no messages without READ_MESSAGE_HISTORY (`message.mdx:904`), bulk delete 2-week rule (`message.mdx:1232`, error 50034), 3 s / 15 min interaction tokens, mutually exclusive gateway vs endpoint delivery, interaction endpoints outside the global limit, 10,000 invalid requests per 10 min, search `total_results`/110000/offset 9975 (changelog 2026-03-19), forwards with at most 1 immutable snapshot, signed CDN URLs, 50 req/s global, no app Delete Guild.
- Marked (unverified): per-server DM setting prevalence; account deletion leaving "Deleted User"; live Developer Terms currency; SRB holding; Discord minimum age; locked-toggle behaviour.
- GDPR: added Art. 9, Art. 14 content and timing, Art. 21 not window-bound, Art. 17 cache reach, a theater/not-theater split, a nuance on Lindqvist, a nuance on Pollis's position plus an Art. 28 processor agreement, and minors.
- Open decisions 2, 3, 10: proposals revised (opt-in default; no-gateway v1; one ticket one PR handling). 3 and 10 **resolved** (Open decisions above).
- pollis-core: `commands/import/` does not exist yet (#1179 has not landed); the draft wrote "(from #1179)" as if it did. **Resolved (shared §13):** #1179's PR builds it.
- Added after reconciliation: `group_import.created_by` was said to match `group_emoji.created_by`, but teardown deletes `group_emoji` rows by creator (`teardown.rs:307-313`). **Resolved (shared §11):** `import_archive.created_by` is NULLed once live; staging imports of a deleted importer are deleted.

### Verified as stated (no change)

- One MLS group per group, no per-channel ACL: `000019_pinned_messages.sql:4-5`, `mls.md` group catch-up section (`mls_group_id == group_id`); `channels` has no position or category column (`000000_baseline.sql:15-22`).
- Kpin semantics: random per MLS conversation, wrap re-done at every epoch from the cache, CAS on `(generation, epoch)`, late joiners read all pins, never rotates (`000019:1-50`, `mls.md:622-660`). No setting disables pins (grep finds none), so "pins disabled" is not a failure mode.
- Wrapping K under `HKDF(Kpin, label || import_id)` is sound with domain separation. Hygiene note: pins use Kpin directly as an AES key while imports use it as HKDF input. That is acceptable but worth one line in the whitepaper.
- Attachments convergent (`r2.rs:706,755,801`), liveness via `live_ref_exists!` (`messages.rs:1913-1921`); `R2_MEDIA_MAX_BYTES` = 104 MiB (`pollis-api/src/broker.rs:99`).
- `foreign_keys=OFF` in production (`teardown.rs:8`); `conversation` table with `kind = 'group'` exists (000016) and every group row is claimed in it (000017), so `group_import_group_only` works. Superseded: the registry is append-only, so it also holds deleted groups' ids; shared `import_archive_insert_gate` checks `groups` instead (shared §10).
- 000035 is the newest migration, so 000036 is next. No existing table or endpoint uses `import`, `group_import`, `group_reservation`, or `/v1/imports`. Endpoint paths follow the `/v1/<resource>/<verb>` and `/v1/read/<resource>` convention.
- `export.rs:22-24` "No re-import" quote; whitepaper §6.8 (`:393-395`) and §13 item 7 (`:709`) omit pins as pre-join content.
- Maestro flow names are kebab-case (`mobile/.maestro/flows/`), so `import-claim.yaml` matches convention.

### Unresolved concerns

- Legal: Developer Terms 5(b)(iii) and 9(b) may make keeping any non-opted-in author's content non-compliant regardless of design. This is a ship gate, as before. **Still open.**
- Kpin never rotates. A removed member who extracted Kpin, plus a DS/R2 breach, reads every future import (`mls.md:656-660`). Wrapping K under Kpin widens the existing #99 residual from pins to whole histories. Open decision 4 should be decided before this ships, not after. **Resolved for v1 (shared §4.3):** decided not to rotate; documented residual, CAS-updatable wrap.
- Admin-trust ceiling: the admin can fabricate content and can redeem any seat with a sockpuppet. `claim_proof` defends against the DS, not the admin. Documented, not fixable without a Discord-aware third party. **Documented (shared §1, §16).**
- Erasure liveness: per-author erasure needs a live admin (or claimant) device. With every admin gone, an Art. 17 request can only be met by deleting the whole import. **Still open;** claimants can erase their own records without an admin (shared §8).
- Gateway desk: if kept, it needs a CLAUDE.md exception written into the same PR, and the "Exclude" button must not be its only consent path. **Resolved for v1:** no desk; consent and seat requests use reactions (steps 2, 5.2).
- The 3-second interaction deadline with lazy link minting (a DS round trip) needs the deferred response every time; test it against a slow DS. **Deferred** with the desk.
- Mobile: claims, viewing, and rewrite reach phones only in a new store build. Old builds can redeem, because the seat stamp is server-side, but cannot render. Maestro flows must be run on a Release build against the dev DS, not only written. **Resolved (shared §15):** re-opening the seat link after updating returns the `SeatGrant`, so old-build redeemers can still claim.
- Storage: 5M-message guilds with attachments are hundreds of GB of R2 for one group. Quota and tier (Open decision 8) block launch for large guilds. **Still open (shared §18).**

### Reconciliation with slack-import-design.md

Both docs claimed migration 000036 and the same scaffolding under different names. The reconciled version below is **resolved: adopted in `docs/import-history-design.md`**, with these deviations decided there: `manifest_ver` dropped (write-once manifest, shared §3.3); `source_account_mac` not in the migration (shared §5.1); `seat_sig` and `minted_by` added (shared §5.1); `source_ref` input fields length-prefixed rather than 0x00-separated (shared §2); link form decided as `pollis://invite/<token>` plus bare token, no fragment form and no `extract()` change (shared §5.2); annotations also carry `release` and `corroborate` kinds (shared §8); `import_archive.scope` added for personal archives (shared §10). The table is kept as the record of how each conflict was settled:

| Shared part | discord-import-design.md | slack-import-design.md | Reconciled |
|---|---|---|---|
| Migration | `000036_group_import.sql` | `000036_import_archive.sql` | One file, `000036_import_archive.sql`, landed once in the scaffolding PR; the second provider adds no migration (or 000037 if it needs one) |
| Archive table | `group_import`, state `uploading`/`published`, inline `wrapped_key`/`wrap_nonce`, `manifest_r2_key`, `published_at` | `import_archive`, status `staging`/`live`, `manifest_key`, `manifest_ver` | `import_archive(id, group_id, created_by, status IN ('staging','live'), manifest_key, manifest_ver, created_at, published_at)`, with Discord's one-way publish trigger and group-only trigger |
| Key wrap | inline on the archive row | `import_key_wrap(import_id, conversation_id, wrapped_k, nonce)`, insert-only | Separate `import_key_wrap(import_id, wrap_target, wrapped_k, nonce, kek_kind IN ('kpin','personal'))`, needed for Slack's personal archives. Mutable only by CAS, so a future Kpin rotation (Open decision 4) can re-wrap; "insert-only" would block that |
| Wrap KDF | `AES-256-GCM(HKDF(Kpin, "pollis/import-wrap/v1" \|\| import_id), K)` | unspecified | Discord's, with `wrapped_k` length fixed at 48 |
| Objects | `group_import_blob` + `manifest_r2_key` | `import_object(r2_key PK, import_id, size_bytes)` for every object | `import_object` as the single liveness registry the broker's delete gate checks, plus `import_chunk` for versioned chunks |
| R2 family | `import/<import_id>/<blob_id>.enc` | `import/<import_id>/<hex64>.enc` | `R2Family::Import`, `import/<import_id>/<hex64>.enc`, never in `live_ref_exists!` |
| Chunk format | JSON, compressed, 1 MiB target, cap 2 MiB, random `chunk_id`, key `HKDF(K, "pollis/import-chunk/v1" \|\| chunk_id)`, leaf-hash list in manifest | CBOR, 64 KiB padding buckets, per conversation per month about 2 MiB, key `HKDF(K, import_id \|\| chunk_id)`, AAD `import_id\|\|chunk_id\|\|version`, Merkle root | Deterministic CBOR (RFC 8949 §4.2; JSON has no canonical form for leaf hashing), random `chunk_id`, 64 KiB buckets, cap 2 MiB plus padding enforced by CHECK, labelled HKDF over `import_id \|\| chunk_id`, AAD including `version`, Merkle root per chunk in the importer-signed manifest |
| Source identity | `source_ref = HMAC(HKDF(K, "pollis/import-source-ref/v1"), discord_user_id)` | `source_identity_hash = HMAC(K-derived salt in manifest, provider\|\|team\|\|user)` | Column `source_ref`; `HMAC-SHA256(HKDF(K, "pollis/import-source-ref/v1"), provider \|\| 0x00 \|\| namespace \|\| 0x00 \|\| account_id)` (namespace = Slack team id, empty for Discord) |
| Seat table | `group_import_seat(link_id PK, import_id, source_ref)`; redeemer from the redemption audit | `group_reservation(id, link_id UNIQUE, import_id, source_identity_hash, email_mac, source_account_mac, sealed_hint, claimed_by, claimed_at)` | `import_seat(id, link_id UNIQUE NULL, import_id, source_ref, proof_hash, email_mac NULL, source_account_mac NULL, redeemed_by, redeemed_at)`. The stamp goes on the seat inside the redeem transaction (Slack's approach). Drop `sealed_hint`: names already live in the K-encrypted manifest. Both docs miss the `is_member` short-circuit (`groups.rs:1687-1695`) |
| Seat link | max_uses = 1, expiry not enforced | max_uses = 1, `expires_at NOT NULL` | Trigger: `max_uses = 1`, `uses = 0`, `revoked_at IS NULL`, `expires_at IS NOT NULL`, same group |
| Redeem response | not specified | `#[serde(default)] reservation: Option<ReservationGrant>` | `#[serde(default)] seat: Option<SeatGrant { import_id, seat_id, source_ref }>` |
| Claim table | `group_import_claim(id, import_id, source_ref, user_id, signed_by_device, signature, withdrawn_at)`, live-unique per source and per user | `import_claim(reservation_id PK, user_id, identity_version, signature, withdrawn_at, withdraw_sig)` | `import_claim(seat_id PK, import_id, user_id, identity_version, claim_proof, signature = 2420 B, withdrawn_at, withdraw_sig)`: Slack's key and fields plus `claim_proof`; trigger requires the seat's redeemer, current membership, and a live import |
| Claim message | `("pollis/import-claim/v1", group_id, import_id, source_ref, user_id, issued_at)` | `ImportClaim` with `identity_version`, fields unspecified | CBOR `("pollis/import-claim/v1", group_id, import_id, seat_id, source_ref, user_id, identity_version, claim_proof, issued_at)`; withdrawal signs `("pollis/import-claim-withdraw/v1", seat_id, withdrawn_at)` |
| FKs | none relied on (triggers) | `REFERENCES ... ON DELETE CASCADE` throughout, and "collected exactly when its import row goes (cascade ...)" | No FK is relied on: production runs `foreign_keys=OFF` (`teardown.rs:8`), so Slack's cascades are inert. Every invariant is a CHECK, UNIQUE, or trigger, and every table goes into `purge_group`/`purge_user_rows` |
| Redaction | claimant rewrites own chunk leaves directly | `import_annotation` applied later by admin devices | Claimant rewrites their own leaves (no admin liveness dependency; clients enforce leaf/root rules); `import_annotation` kept for disputes only |
| Endpoints | `/v1/imports/{create,chunks/put,blobs/put,publish,chunks/rewrite,delete}`, `/v1/import-seats/create`, `/v1/import-claims/{create,withdraw}`, `/v1/read/{imports,import-chunks,import-claims}` | `/v1/imports/{create,key-wrap,object/register,chunk/put,finalize,delete,claim,claim/withdraw,annotate}`, `/v1/reservations/{create-batch,revoke}`, reads `imports/list` etc. | `/v1/imports/{create,key-wrap,objects/register,chunks/put,publish,delete}` (`chunks/put` carries `expected_version` and covers rewrite), `/v1/import-seats/{create,attach-link}`, revoke via existing `/v1/invite-links/revoke`, `/v1/import-claims/{create,withdraw}`, `/v1/import-annotations/create`, reads `/v1/read/{imports,import-chunks,import-claims,import-annotations,import-seats}` (seats: admin only), matching the existing `/v1/<resource>/<verb>` and `/v1/read/<resource>` style |
| Join path | external join (correct, `invites.rs:334-338`) | "the joiner's Welcome lands" through reconcile | External join; Kpin adopted lazily. slack-import-design.md's Welcome wording is wrong for link redemption |
| Link URL | `pollis://join/...` (wrong) | fragment form only (I15) | Today's form is path-based, and `extract()` discards fragments (`invite_token.rs:129`). Fragment-only links need a code change to `extract`/`invite_url` and the deep-link route; decide once for both |
| Maestro flows | `import-claim.yaml`, `import-history.yaml` | `import_claim.yaml`, `import_history.yaml`, `import_redact.yaml` | kebab-case, the existing convention: `import-claim.yaml`, `import-history.yaml`, `import-redact.yaml` |
| Ticket and PR | "same PR or follow-up on the same ticket" | #1179 "owns the shared scaffolding" | Scaffolding lands once, in a rescoped #1179 PR; #1180 builds on it in its own PR; the tickets' "never its messages" text is amended first |

