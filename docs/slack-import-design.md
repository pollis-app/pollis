# Slack import with message history: research and design

Related: #1179 (structure import; its PR lands the shared scaffolding), #1180 (Discord), #1181 (history keeper), #847 (invite links), #99 (pin key), #856 (account export), #720 (envelope retention), #872 (bots as members).

**Shared scaffolding** (archive format, keys and the `Kpin` wrap, the waiting state, seats, claims, redaction and erasure, the `import/` R2 family, migration 000036, DS endpoints, shared pollis-core commands, shared invariants, mobile parity, backward compatibility, docs and principle changes) is **defined in the shared import spec, `docs/import-history-design.md`**. This file covers only what is specific to Slack. Section references of the form "shared §N" point there.

Research note: slack.com, docs.slack.dev and api.slack.com could not be fetched directly from this environment (DNS/egress refused). Every Slack claim below comes from search-indexed copies of the official pages, cited by their official URL. Claims that rest only on a third-party source are marked **(secondary)**. Anything marked **(verify)** must be re-read against the live page before the PR lands.

## Summary

- **Content comes from the Slack export ZIP, not the API.** The API route is either infeasible (a Pollis-distributed app is clamped to 1 request/minute and 15 messages per call on `conversations.history`/`conversations.replies`, about 1.4 years of wall-clock for a 5-year, 200-person workspace) or prohibited (Slack's API Terms bar distributed apps from bulk-exporting message data). The customer's own internal app is unclamped and is still needed, but only for what the ZIP lacks: emails, emoji, user groups, and optionally sending each person their link. Caveat: Slack's May 2025 terms update names "templatized one-off custom apps" as commercial distribution that belongs in the Marketplace ([tos-updates changelog](https://docs.slack.dev/changelog/2025/05/29/tos-updates)), so a Pollis-supplied manifest that every customer installs may itself count as a distributed app. That is a legal question (Open decision 1), and it bites hardest on Phase C, which reads history through that app.
- **Admins import public channels only.** Pollis has no private channels: every channel of a group shares one MLS group and `channel_type` is `'text' | 'voice'`. #1179's mapping `is_private -> channel_type` is wrong, and if implemented it would show private Slack channels to the whole group. Private channels and DMs are imported by **their own participants**, using their own Slack token, either into a new group (a private channel) or into a personal archive (DMs). An admin never uploads anyone's DMs.
- **Key distribution, seats, claims and redaction are shared** (shared §4-§8). Slack adds two things: email-bound seats (Slack exports and `users:read.email` can supply emails, Discord never can), and personal archives for Phase C DMs (shared §4.4).
- **Email serves as an optional binding MAC, never a lookup.** A server-side email index would give the operator the email of every person in the source workspace who never joined Pollis (shared §5.4).
- **Source deletion is a retention policy or channel deletion, never chat.delete at scale.** Legal holds, eDiscovery archives, Slack Connect counterpart copies and already-downloaded exports all survive. The UI says "your Pollis copy is encrypted; here is how to take Slack's copy down", never "erased".

## Scope change vs #1179

| #1179 says | This proposal | Why |
|---|---|---|
| "never its messages" | Public channel history is imported by an admin. Private channels and DMs are imported by their participants. | Owner decision; the ticket text is amended before code (shared §0). |
| `is_private -> channel_type` | Private channels never become channels of an existing group. A participant can import one as a **new group**, or skip it. | `channel_type` is `'text' \| 'voice'` (`frontend/src/types/index.ts:63`, `mobile/types/index.ts:35`); there is no channel ACL table (baseline `channels.channel_type TEXT NOT NULL DEFAULT 'text'`, `pollis-schema/migrations/000000_baseline.sql:20`, no CHECK); all channels share one MLS group (`.codesight/wiki/mls.md:3`, `000019_pinned_messages.sql:3-4`). |
| Scopes `channels:read, groups:read, users:read, users:read.email, emoji:read, usergroups:read` | Same, plus `team:read`. Optional `chat:write`, `im:write` for link delivery. Participant imports use user scopes `im:history`, `mpim:history`, `groups:history`, `files:read`. | See Research §2 and §5. |
| "DS must not learn that an import happened beyond the writes" | The DS necessarily stores an opaque archive and seat rows. It never learns the provider, the source workspace, channel names, or any email (shared §1). | |
| Members: invite plan via `send_group_invite` to matched emails | Single-use seat links for everyone, including people with no Pollis account (shared §5). Matched existing users may still get the ordinary invite. | A seat must carry the source identity it claims. |
| Shared scaffolding: provider trait, preview/plan/apply, report | Also the archive, seats, claims, key handoff and redaction, all in `docs/import-history-design.md`, landing in this ticket's PR. | Discord has no emails; everything else is the same. |
| Mobile: maybe exempt | The admin import job is exempt with a stated reason. Everything on the member side and every short admin action ships on mobile (shared §15). | See "UI". |

#856: this importer is an import path, which `export.rs`'s "No re-import" line forbids. The owner's knowing revision, the I24 refusal of export files, and the honest admission that a scripted conversion of `pollis-export.json` into Slack-ZIP shape is a working restore path are in shared §3.5 and §16.

## Research findings

### 1. Workspace export (the ZIP)

| Plan | Public channels | Private channels, DMs, group DMs | Who | Notes |
|---|---|---|---|---|
| Free | Yes | No | Workspace Owners/Admins | Can only export **links to files from the last 90 days** ([export help](https://slack.com/help/articles/201658943-Export-your-workspace-data)). Visible history is 90 days; content older than one year is permanently deleted ([usage limits](https://slack.com/intl/en-by/help/articles/115002422943)). |
| Pro | Yes | Only by **applying to Slack**. Slack rejects applications that lack valid legal process, consent of members, or a requirement or right under applicable law ([import/export guide](https://slack.com/help/articles/204897248-Guide-to-Slack-import-and-export-tools)). | Workspace Owners | A Pro workspace upgraded from Free does not export data older than one year that was deleted before the upgrade (same guide). |
| Business+ | Yes | Owners apply for a **self-serve full export** tool; it is usable only after approval ([export help](https://slack.com/help/articles/201658943-Export-your-workspace-data), [data management](https://slack.com/trust/data-management)). | Workspace Owners | The owner must confirm that employment agreements and corporate policies are in place ([guide](https://slack.com/help/articles/204897248-Guide-to-Slack-import-and-export-tools)). |
| Enterprise (Grid) | Yes | Full export available to Org Owners; single-user exports and scheduled exports are Enterprise-only ([export help](https://slack.com/help/articles/201658943-Export-your-workspace-data)). | Org Owners | The members file is `org_users.json` ([how to read exports](https://slack.com/help/articles/220556107-How-to-read-Slack-data-exports)). |

- **Free plan deletion policy.** Slack announced in late June 2024 that from **26 August 2024** messages and files in free workspaces older than one year would be deleted on a rolling basis, irrecoverably, even after a later upgrade ([heise](https://heise.de/en/news/Slack-deletes-data-from-free-workspaces-after-one-year-9783229.html); the forwarded customer notice dated 27 June 2024 is at [lists.osgeo.org](https://lists.osgeo.org/pipermail/uk/2024-July/001281.html)). As a result, a Free workspace can import at most the last year, and probably only the last 90 days: Slack says history beyond 90 days "will become available should you upgrade" ([free workspace history](https://slack.com/help/articles/29414264463635)), and secondary sources say a Free export covers 90 days ([usehall](https://usehall.com/guides/how-to-export-slack-conversations)). Plan for 90 days on Free **(verify)**.
- **Layout.** `channels.json`, `users.json` and `integration_logs.json` appear in every export type. `groups.json` (private channels), `dms.json` and `mpims.json` appear only in full exports. There is one folder per conversation holding per-day JSON files, and a `canvases` folder that covers only public channels in a public-only export ([how to read exports](https://slack.com/help/articles/220556107-How-to-read-Slack-data-exports)). Each message object carries `ts`, `user`, `text`, `thread_ts`, `reply_count`/`replies`, `reactions[{name, users, count}]`, `edited{user, ts}`, `files[]`, and a `subtype` for joins, leaves, topic changes, bot messages and so on **(secondary, standard across importers such as [slack-export-viewer](https://pypi.org/project/slack-export-viewer/0.1.2/))**. Thread replies sit inline in the per-day files, so the ZIP route needs no `conversations.replies` calls.
- **Emails are usually absent from `users.json`.** Mattermost's docs and Zulip's import guide both say a standard export lacks user emails, and both require a bot token with `users:read.email` to fetch them ([Zulip](https://zulip.ipd.kit.edu/help/import-from-slack), [Mattermost forum](https://forum.mattermost.com/t/bulk-import-from-slack-fails-with-message-error-bulkimport-some-users-not-found/17656)). Where emails do appear, the workspace's "Email Display" setting controls them ([Roam](https://ro.am/support/slack-import-missing-emails), secondary). The API requires `users:read.email` for the `email` field on `users.list`/`users.info` ([users.list](https://docs.slack.dev/reference/methods/users.list/)).
- **Files are links, not bytes.** "Data exports will include links to files, but not the files themselves" ([guide](https://slack.com/help/articles/204897248-Guide-to-Slack-import-and-export-tools)). The links carry an export token (`t=xoxe-...`) appended to `url_private`/thumbnail URLs ([slackdump docs](https://github.com/kolsys/slackdump/blob/master/doc/usage-export.rst); Zulip notes the `xoxe-` token, [Zulip](https://zulip.ipd.kit.edu/help/import-from-slack)). An owner can revoke those tokens from the exports page after ingest ([DISCO](https://support.csdisco.com/hc/en-us/articles/18024077825933-Ingesting-Slack-exports-directly-into-Ediscovery), secondary) **(verify)**. Two consequences: **anyone holding the ZIP can download every file until the token is revoked**, and the links stop working once the files are deleted in Slack (retention policy, deletion), so files must be fetched before any source deletion.
- **Slack Connect.** In a standard workspace, links to files in shared private channels or cross-org DMs are included only if a member of your own workspace shared them. In Enterprise orgs, files from Connect channels are included ([guide](https://slack.com/help/articles/204897248-Guide-to-Slack-import-and-export-tools)).

### 2. API route

- **Scopes.** `conversations.history` needs `channels:history`, `groups:history`, `im:history` or `mpim:history` by conversation type. A **user token reaches only the conversations that user is a member of**; a bot token reaches only conversations the bot is in ([conversations.history](https://docs.slack.dev/reference/methods/conversations.history/)). No token lets an app read DMs between two other people. The API route therefore cannot replace a full export for DMs or private channels the token holder is not in. That is a hard limit, and it also points to the right consent model (§ "End-to-end flow", participant imports).
- **Tiers.** Tier 1 is 1+/min, Tier 2 is 20+/min, Tier 3 is 50+/min. Limits apply per method, per workspace, per app ([rate limits](https://docs.slack.dev/apis/web-api/rate-limits)). `conversations.list` is Tier 2 (same page). `users.list` is Tier 2 and `users.info` is Tier 4 **(secondary, [resumelens](https://www.resumelens.org/blog/slack/slack-rate-limits-and-best-practices))**.
- **The 2025 change.** For **commercially distributed apps not approved for the Marketplace** ("unlisted"), `conversations.history` and `conversations.replies` move from Tier 3 to Tier 1: **1 request per minute, at most 15 objects per request**. This applied to new apps and new installations from **29 May 2025**. For existing installations of unlisted apps, the date was originally 2 September 2025 and was revised to **3 March 2026**. **Internal customer-built apps are not affected**: custom apps keep 50+ requests/minute, with a `limit` maximum and default of 1,000 (the default-of-1,000 figure is **(unverified)**; the method page historically documents a default of 100). Marketplace apps are not affected ([changelog](https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps/), [api.slack.com copy](https://api.slack.com/changelog/2025-05-terms-rate-limit-update-and-faq)). A clamped app sees `ratelimited` with `Retry-After` of about 60 ([allanninal.dev](https://www.allanninal.dev/slack/non-marketplace-history-clamp/), secondary).
- **What this means for Pollis.**
  - *Pollis distributes an app (OAuth "Add to Slack").* The app is clamped. On top of that, the API Terms (§3) bar distributed apps from bulk-exporting message data. Marketplace listing lifts the rate clamp, but Marketplace apps are still third-party distributed apps under the Terms, so a migration-to-competitor app is still bulk export, and Slack's Marketplace guidelines list apps that export or back up message data as unsuitable for listing (per search snippet of [Marketplace guidelines](https://docs.slack.dev/slack-marketplace/slack-marketplace-app-guidelines-and-requirements/)). **Dead end on both axes.**
  - *The admin creates their own internal app from a Pollis-supplied manifest.* The customer owns the app; it is not distributed, so it is unclamped (50+/min, 1,000 per page). Slack's developer messaging says customers building custom apps for internal use keep full access to their data (as reported by [Computerworld](https://www.computerworld.com/article/4005509/salesforce-changes-slack-api-terms-to-block-bulk-data-access-for-llms.html), secondary). The token stays on the admin's device (#1179). Legal review is still needed on one point: does Pollis's client acting on the customer's internal app count as "providing an application" under the Terms? Pollis never holds the token and never receives the data in plaintext, which is the strongest available position. Against it: the May 2025 terms update says the Marketplace is the only appropriate channel for commercially distributing apps "whether those apps are unlisted ... or templatized one-off 'custom' apps" ([tos-updates](https://docs.slack.dev/changelog/2025/05/29/tos-updates)), and the December 2024 developer policy update extends the restriction to parallel custom apps distributed beyond your organization ([dev policy update](https://docs.slack.dev/changelog/2024/12/10/dev-policy-update)). A manifest Pollis ships to every customer reads uncomfortably like a templatized custom app. This is an **Open decision**, not a settled matter, and the rate clamp may not even be the binding constraint: Slack decides what counts as internal.

### 3. Slack API Terms, developer policy, Discovery API

- The current API Terms (effective 10 October 2025 per indexed copy, **(unverified)**: the dates Slack's own changelog gives are 29 May 2025 for new apps and 30 June 2025 for existing ones, [tos-updates](https://docs.slack.dev/changelog/2025/05/29/tos-updates)) prohibit **providers of applications distributed outside their own organization** from training LLMs on API Data and from **bulk exporting Slack message and file data** unless an additional agreement allows it, with Discovery API use for approved security and compliance cases as the named exception ([API Terms](https://slack.com/terms-of-service/api)). Law-firm summaries of the May 2025 revision also describe a ban on "persistent copies, archives, indexes or long-term data stores" ([Hunton](https://www.hunton.com/insights/legal/salesforce-locks-down-slack-data-time-to-review-your-slack-api-terms)). That wording could not be found in the current terms text **(verify)**. For apps created before 29 May 2025 the terms took effect on 30 June 2025 (per Slack's changelog as summarized in search results).
- **The export ZIP is not "API Data".** It is the customer's own export feature, governed by the customer's agreement with Slack. Importing a ZIP the customer downloaded is the cleanest legal position available. This is a further reason to make the ZIP the content source.
- **Discovery API.** It is available on Enterprise; an Org Owner requests enablement; it is restricted to security and compliance use cases (eDiscovery, archiving, DLP); and access goes to listed partners ([Discovery API guide](https://slack.com/help/articles/360002079527-a-guide-to-slacks-discovery-apis)). Migration to a competing messenger is not a listed use case. **Pollis cannot use it.**
- **AI/LLM.** Not relevant to the import, because no plaintext leaves the admin's device. Pollis must never route import content through a model. That would also end the E2EE story (the same reasoning as #1181 "Later").

### 4. Identity

- User IDs: `U...` are unique within a workspace. `W...` (enterprise user IDs) are unique across an Enterprise org, and some users have both. `migration.exchange` maps local IDs to global ones ([user type](https://api.slack.com/types/user), [Enterprise dev guide](https://docs.slack.dev/enterprise/developing-for-enterprise-orgs), [migration.exchange](https://docs.slack.dev/reference/methods/migration.exchange)). **Source identity key: `(provider, enterprise_id or team_id, W-id if present else U-id)`.**
- Slack Connect external users carry a foreign team. They appear in shared-channel history but not in your `users.list`. Treat them as source identities with `external = true`. They are unclaimable unless the admin chooses to issue them a seat (see "Where this breaks").
- **Sign in with Slack** is OpenID Connect with scope `openid` (plus optional `email` and `profile`), using `/openid/connect/authorize`, `openid.connect.token` and `openid.connect.userInfo` ([SIWS](https://docs.slack.dev/authentication/sign-in-with-slack/)). The ID token's Slack user and team claims (`https://slack.com/user_id`, `https://slack.com/team_id`) are from memory and **(verify)**.
- **PKCE is GA** (Slack changelog dated 30 March 2026). Public clients can complete OAuth without a client secret through `oauth.v2.access`. Enabling it is one-way (undoing it needs Slack support); refresh tokens last 30 days; custom-URI-scheme redirects always get rotating tokens; "desktop redirects cannot request bot scopes" is **(unverified)**, search snippets of the pages below do not show it ([using PKCE](https://docs.slack.dev/authentication/using-pkce), [changelog](https://docs.slack.dev/changelog/2026/03/30/pkce)). This matters because the redemption binding and participant imports run on members' devices, where no client secret can live. Whether PKCE also covers `openid.connect.token` is **(verify)**. If it does not, use `oauth.v2.access` with a minimal user scope and read `authed_user.id` and `team.id` from the response.
- **DM delivery of links.** `chat.postMessage` is a special tier: about 1 message per second per channel, with a workspace-wide ceiling of several hundred per minute ([rate limits](https://docs.slack.dev/apis/web-api/rate-limits), [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/)). Sending 200 DMs takes minutes. The bot needs `chat:write`; it opens the DM with `conversations.open` (`im:write`). Pass `unfurl_links: false`.

### 5. Deletion on Slack

- `chat.delete` with a **user token deletes only what that user could delete in Slack**. A bot token deletes only the bot's own messages. Impersonated posts cannot be deleted ([chat.delete](https://docs.slack.dev/reference/methods/chat.delete/)). Owners and admins can delete others' messages in public channels, private channels and group DMs **they are part of**; there is **no bulk delete**; deleting a channel removes all its messages ([edit or delete messages](https://slack.com/help/articles/202395258-Edit-or-delete-messages)). No one can delete two other people's 1:1 DMs except through retention.
- **Retention.** Paid plans offer: keep everything with edit/delete tracking, keep everything without tracking, or a custom deletion timeline. A daily job deletes expired content permanently, including pinned items. On Enterprise, org-level policies constrain workspace owners, and a new policy needs Primary Org Owner approval ([retention](https://slack.com/help/articles/203457187-Customize-message-and-file-retention)). Free plans have no custom retention; the rolling one-year deletion is the only mechanism, short of deleting the workspace.
- **Legal holds** (Enterprise) preserve all of a held member's messages and files regardless of retention settings, edits or deletions ([legal holds](https://slack.com/help/articles/4401830811795)). A hold overrides retention until it is released ([Logikcull](https://www.logikcull.com/blog/slack-legal-hold), secondary). Data already in an eDiscovery warehouse stays there ([retention](https://slack.com/help/articles/203457187-Customize-message-and-file-retention)).
- **EKM** (Enterprise) encrypts with customer keys in AWS KMS. Access can be revoked at org, workspace, channel, time-frame and file level ([EKM](https://api.slack.com/enterprise-key-management)). For EKM customers this is the strongest "take it down" lever: revoking the key makes Slack's copy unreadable to Slack. Pollis should mention it, and it needs no API.
- What no deletion guarantees: Slack backups (no published deletion SLA found), held data, eDiscovery copies, the other org's copy of Connect channels, every export ZIP already downloaded (including the one used for this import), and integration copies (webhooks, other apps).

### 6. MCP

Slack's official hosted MCP server went GA on 17 February 2026 alongside the Real-Time Search API, after a limited release from October 2025 ([Slack blog](https://slack.com/blog/news/mcp-real-time-search-api-now-available), [MCP overview](https://docs.slack.dev/ai/slack-mcp-server/)). Slack describes RTS as query-based access "without storing customer data on external servers", the opposite of a bulk read path. The endpoint `mcp.slack.com/mcp`, the admin-approval requirement and the exact tool list rest on secondary sources ([hivegate issue](https://github.com/hivegate-ai/hivegate/issues/96), [TrueFoundry](https://www.truefoundry.com/blog/slack-mcp-server)) **(secondary)**; Slack's feature drop also lists write-side tools (drafting, formatting messages), so "search-oriented" is a simplification. **It is irrelevant to this import and should not be used.** It is a search surface for LLM agents, not a bulk read path. It runs as one user, so it sees only that user's conversations. It inherits the same rate limits and Terms. And it puts an LLM vendor in the plaintext path, which defeats the purpose of moving to E2EE. Community MCP servers just wrap the same Web API with the same limits and are no better.

### 7. What is importable

| Slack object | Source | Import? |
|---|---|---|
| Public channel messages, threads, reactions, edits (final text + edited flag), files | ZIP | Yes |
| Pins | ZIP message fields **(verify the field name per export)** | Yes, as a flag inside the archive |
| Join/leave/topic/purpose subtypes | ZIP | As system lines, collapsible |
| Bot/integration messages | ZIP (`bot_message`) | Yes, attributed to the bot name, never claimable |
| Private channels | ZIP (Business+/Enterprise) or a participant's user token | Only by a participant, into a **new group** |
| DMs, group DMs | ZIP (approved full export) or a participant's user token | Only by a participant, into a **personal archive** |
| Canvases | ZIP `canvases/` (public only in public export) | As file attachments, v1 |
| Huddles | No audio is retained; huddle notes are canvases | Notes only, as canvases |
| Clips (audio/video messages) | Files | As files (size cap) |
| Lists | Not documented in export | No, reported |
| Workflows | Exported separately as JSON ([workflows](https://slack.com/help/articles/30011815092499-Export-and-import-Slack-workflows)) | No: Pollis has no workflow engine (#872 scope) |
| Slack Connect channels | ZIP, partially | Off by default (another org's data), reported |
| Edit history, deleted messages | Not in a standard export | No |

## End-to-end flow

Recommendation: **ZIP for content, the customer's internal app for identities, the customer's own decision for deletion.**

### Phase A: admin, desktop, structure + public history

1. **Settings -> Import -> Slack** (a page, no modal). The page states up front what will and will not move, and which Slack plan gives what (table from Research §1).
2. **Create the internal app.** Pollis shows a generated manifest and five steps:
   1. Open `https://api.slack.com/apps` -> Create New App -> From a manifest -> pick the workspace.
   2. Paste the manifest. Bot scopes: `users:read`, `users:read.email`, `team:read`, `emoji:read`, `channels:read`, plus optional `usergroups:read`, and `chat:write` + `im:write` for link delivery. No `*:history` scopes in this phase.
   3. Install to Workspace. If the workspace requires app approval, a Workspace Admin approves it (#1179 "do not work around it").
   4. Copy the `xoxb-` Bot User OAuth Token into Pollis. It is held in memory, or in the keystore with an explicit deadline if the job will span restarts (see "pollis-core").
   5. After the import: uninstall the app (Pollis links to the page).
3. **Provide the ZIP.** Pollis streams it from disk (no full extraction) and records its SHA-256 in the local import journal (`import_job`, shared §7).
4. **Preview (dry run).** Pollis parses `users.json` + `users.list` (1-2 Tier 2 calls), `channels.json`, and per-channel day files, then shows: channels to create or match (#1179's idempotent name match), message counts per channel, files by size class (over-cap files listed), emoji (#1179 rules), source identities (members, deactivated, bots, external), and everything skipped with the reason. Private/DM folders present in a full export are listed as **"will not be imported by you"**, with a count only.
5. **Apply** (`import_apply`, resumable, idempotent, journaled locally): #1179 structure writes (`create_group`, `create_channel`, `upload_group_emoji`), then the shared pipeline (shared §3, §4.1): mint `K`, wrap under `Kpin`, download each referenced file through its export link before anything else and encrypt it, build and upload chunks, sign and publish the manifest, create one seat per claimable source identity. Slack specifics: `source_ref` namespace is `enterprise_id` when present, else `team_id`, and the account id is the `W` id when present, else the `U` id (Research §4); `src_id` is `<channel_id>/<ts>`.
6. **Deliver links** (shared §5.2 link form: `pollis://invite/<token>` plus the bare token, never an https or fragment URL). Option (a): the bot DMs each person their link (`chat.postMessage`, `unfurl_links: false`). Option (b): export a CSV of `email, link` for the admin's own mail tool (`export_import_seat_links_csv`). Pollis never mails it, so the DS never sees the email-to-group mapping. Deactivated users get (b) or nothing. Seats are email-bound by default wherever the admin has an email.
7. **Report.** It covers everything skipped, failed files, unclaimable identities, and a reminder to revoke the export file-link token.

### Phase B: member, desktop or mobile

1. The member opens the link: on mobile `pollis://invite/<token>` opens the app (the #1094 confirm-before-join step stays); on desktop they paste it into the join field (shared §5.2).
2. A member without a Pollis account signs up by email OTP. That flow already exists; the seat does not change it.
3. Optional binding, if the admin enabled it:
   - Email-bound seat: the DS checks the redeemer's OTP-verified `users.email` against the seat's `email_mac` inside the redeem verdict (shared §5.3, §5.4).
   - Slack-bound seat: **unsound as originally drafted.** The draft had the client run PKCE OAuth and *present* `(team_id, user_id)`, with the DS checking a MAC of it. The DS cannot tell a value Slack vouched for from one the client typed, and Slack user ids are visible to every workspace member, so anyone holding a leaked link presents the right pair and passes. A real binding needs the DS to verify a Slack-signed OIDC ID token (signature against Slack's JWKS, `aud` = the customer's app, a `nonce` bound to the link selector), which gives the DS Slack awareness that #1179 forbids. Until the owner decides that trade (Open decision 12), v1 ships email binding only, and the migration has no `source_account_mac` column (shared §5.1).
4. Redemption, external join, the "Imported history will appear when another member of this group opens Pollis" waiting state, claim, rendering, withdraw, redact and dispute are shared (shared §4.2, §5.3, §6-§8). Rendering badge text: "Imported from Slack".

### Phase C: participants import their own private channels and DMs (optional, either platform)

Pollis supplies a **second, user-scope manifest**, or the same app with user scopes added: `im:history`, `mpim:history`, `groups:history`, `channels:read`, `groups:read`, `users:read`, `files:read`. PKCE must be enabled (no secret on devices). Each participant authorizes it with their own Slack account and imports:

- **A private channel:** the participant who imports it becomes admin of a **new Pollis group** holding that channel, a `group`-scope import. Seats are minted for the channel's other members exactly as in Phase A.
- **Their DMs:** into a **personal archive** (`scope = 'personal'`, `K` under the vault-derived KEK, shared §4.4). Only they can read it, on every device. The other party's messages in those DMs are ones the importer already received. It is never shared into a group. When both parties import, each holds their own copy.

Volume is small (one person's conversations), so an unclamped internal app at Tier 3 is ample (see the math in "Where this breaks").
### Phase D: source deletion (admin, after the report is clean)

The deletion tool is a checklist page, not an automated purge:

1. It refuses to proceed while any file in the report is still "pending download".
2. Revoke the export file-link token (Slack exports page).
3. Pick the lever, which depends on plan:
   - Paid: set a custom retention policy, shortest available, for messages and files.
   - Any plan: an owner or admin can delete channels, which removes all their messages. This is the only bulk option on Free.
   - EKM: revoke the keys.
   - Close the workspace.
4. Show what survives: legal holds, eDiscovery copies, Connect counterparts, downloaded exports, other integrations.
5. Optional per-user helper: each user runs `chat.delete` over their own messages with their own user token (Tier 3 throttled, resumable). It is offered, never required.

Wording: "Your Pollis copy is end-to-end encrypted. Slack still holds its copy until you remove it, and some copies (legal holds, eDiscovery, exports you downloaded) outlive any deletion."

## Pollis changes

Schema (migration `000036_import_archive.sql`), DS endpoints, the redeem change, the R2 family, teardown, archive format, key handoff, shared commands, backward compatibility and the docs list are **defined in the shared import spec** (shared §3-§16). The Slack PR (#1179) lands all of them. This section lists only what Slack adds.

### 1. Concept mapping (Slack to the shared record, shared §3.1)

| Slack | Pollis today | Import target |
|---|---|---|
| Public channel | `channels` row, `channel_type 'text'` | Same row; imported history renders above the first live message, behind the shared "Imported history" boundary |
| Thread (`thread_ts`) | `message.thread_id`, framed in-payload (#825, `pad_threaded`) | `thread_root`; rendered in the existing thread panel |
| Reaction | `message_reaction` (server row keyed on `users.id`) | `reactions` with `authors` (Slack exports list reactors), inside the archive only |
| Edit | `type='edit'` envelope | `edited_at`; final text only |
| Pin | `pinned_message` under `Kpin` (#99) | `pinned` flag; the "Imported pins" section (shared §7) |
| Mention `<@U..>` | Pollis mentions (#294 pending) | `<src:SOURCE_REF>`, resolved at render to the claimant or the display name |
| Channel ref `<#C..>` | channel link | `<conv:N>` when imported, plain text otherwise |
| Custom emoji `:x:` | `group_emoji` (#848) | #1179 normalization map applied to text and reactions |
| File | `attachment_object` + `attachment_ref`, convergent | Shared `import/` family, random keys (shared §3.4, §9) |
| `bot_message` | | `kind = bot`, never claimable |
| join/leave/topic subtypes | | `kind = system`, collapsible |
| Canvases, huddle notes | | Files (v1) |

### 2. Email binding: Slack-specific UX

- The MAC construction, the redeem check and what the DS can and cannot learn are shared §5.4. Emails come from `users:read.email` (`users.list`); they stay in the local `import_job` journal until delivery, then are discarded, and never enter the archive.
- UX cost: the binding compares against the redeemer's Pollis `users.email`. A person who signs up to Pollis with a personal address instead of their Slack work address fails with the same opaque rejection as an attacker (shared I11). The seat delivery message and the join page must say "use the address Slack has for you" before redemption, and the admin's seat list offers "re-issue unbound" (`mint_import_seat_links(..., email_bound = false)`, shared §13).
- What the DS learns at redemption of an email-bound seat: that this Pollis account's email equals the source workspace's email for that seat. State it in the whitepaper (shared §16).

### 3. Slack provider (`pollis-core/src/commands/import/slack/`)

- `zip.rs`: a streaming ZIP reader (`zip` crate, read without extracting) implementing `ImportProvider::records` and refusing any input carrying the #856 export marker (shared I24). `api.rs`: a reqwest client to `slack.com/api/*`, honoring `Retry-After`. It is the only place a Slack token exists.
- Commands (Slack-only; everything else is shared §13): `import_slack_preview(zip_path, bot_token) -> ImportPlan`, `send_import_seat_links_slack(import_id, seat_ids)` (bot DMs), `export_import_seat_links_csv(import_id, path)`, `participant_import_slack(oauth_pkce)` (Phase C), `slack_source_deletion_checklist(import_id)` (Phase D).
- Token custody: in memory by default. If the job spans restarts, store it in the keystore (`keystore.rs` backend) under `import_token_{import_id}` with a deadline, and delete it on completion, cancel, or the deadline. It is never written to the local DB, logs or the report, and never sent to the DS (S1).

### 4. UI (Slack-specific; shared member and seat UI is shared §7, §15)

- **Desktop** (`frontend/`): Settings -> Import -> Slack page (wizard steps as sections, progress, report), registered in `router.tsx`, `PAGE_RESULTS` in `SearchPanel.tsx`, and the Sidebar nav; the Phase D deletion checklist page; Phase C entry ("Import my Slack DMs / a private channel").
- **Mobile** (`mobile/`): the Phase D checklist (read and tick, no Slack calls needed), Phase C participant DM import and private-channel import into a new group (small volume, the PKCE redirect works on mobile).
- **Exemption claimed (state it in the PR):** only the Phase A admin job (multi-GB ZIP, resumable file downloads that may last hours to days; iOS gives no background-execution guarantee). This is the "makes no sense there" exemption, narrowed to the long-running job (shared §15).
- E2E: shared flows (shared §15) plus Playwright `e2e/import-slack.spec.ts` (preview and apply against a fixture ZIP).

### 5. Docs (Slack-specific, beyond shared §16)

The import page's plan table (Research §1), the controller checklist (Where this breaks 5), and a short `docs/` note on the internal-app manifest and its Terms caveat (Open decision 1).
## Invariants and tests

Shared invariants I1-I28 (seats, claims, chunks, objects, teardown, rendering, #856 refusal, link form) are **defined in the shared import spec** (shared §14). Slack adds:

| # | Invariant | Enforced by | Test |
|---|---|---|---|
| S1 | A Slack token never reaches the DS, the local DB, logs or the report | `import/slack/api.rs` is the only token holder; the token type is not `Serialize`; no pollis-api type has a token field (shared I23) | Harness asserts no DS request body contains `xox`; `slack_token_never_serialized` |
| S2 | A private Slack conversation cannot target a group-wide channel | `ImportTarget` enum: `GroupChannel` is constructible only from `SourceKind::Public`; private sources produce `NewGroup` or `Personal` | Unit test (compile-time via the type, plus a property test over plans) |
| S3 | DMs are never imported by an admin | Phase A plan has no `Personal` or DM target; DM folders are counted only | `import::slack::tests::admin_plan_never_contains_dms` |
| S4 | Every referenced file is downloaded (or recorded failed) before publish, so source deletion cannot strand one | apply ordering; the deletion checklist refuses while any file is pending | `import::slack::tests::publish_waits_for_file_downloads` |
## Where this breaks

1. **The admin can fabricate content, then watch people claim it.** The importer holds the plaintext and signs the archive, and nothing ties a record to Slack. A claim attests the account, not the content. Mitigations, all partial:
   - Every imported message permanently shows "imported by @admin on <date>, not verified by its author".
   - A claimant can dispute any record, and the dispute renders to everyone.
   - The claim UI shows the claimant a per-channel count of "your" messages before they sign.
   - A future content check: compare against Slack's own copy before source deletion (the claimant samples their own Slack messages through Phase C OAuth and the client diffs them).

   None of this makes fabrication impossible. Say so in the UI and the whitepaper.
2. **Email-bound seats are only as strong as the mailbox.** With a corporate address, the employer's IT controls the mailbox and therefore can pass Pollis OTP for it. An admin can therefore claim an employee's seat. With Slack OAuth binding, the Slack admin, or an IdP admin under SSO, can often sign in as the user. **Every binding in an employer workspace ultimately binds to the employer.** It defends against link leakage, not against the employer. This is acceptable for public-channel history the employer already holds. It is the reason DMs are never admin-imported. A later claimant who finds their seat taken files a dispute (shared §8), and the admin can revoke the seat (shared §6).
3. **A mailbox can change hands.** Email reuse after someone leaves, or a recycled personal domain. Seats expire (`expires_at` is required) and are single-use, so the window is bounded. Re-issue after expiry is a deliberate admin action.
4. **The link travels through Slack.** Slack stores the DM, so Slack, and anyone who later receives a full export, has every link. The `pollis://` form with `unfurl_links: false` defeats unfurl fetches and web-server logs (shared §5.2, I28). Email binding defeats replay by a third party (the drafted Slack binding does not; see Phase B step 3). An unbound seat does not: whoever opens it can claim the source identity and have that person's history rendered under their own name. Default to email-bound wherever the admin has an email, and show unbound seats as such in the admin list. The links lose value once Slack's copy is deleted in Phase D, and expired seats are worthless.
5. **Non-consenting and never-moving people (GDPR, works councils).** Public channel messages of people who never join Pollis are processed into a new system: the employer is controller, Pollis is processor of ciphertext (GDPR [Art. 28](https://gdpr-info.eu/art-28-gdpr/)).
   - The employer needs a lawful basis ([Art. 6(1)(f)](https://gdpr-info.eu/art-6-gdpr/) legitimate interest is the usual one for business records).
   - It must notify data subjects, including departed ones ([Art. 13](https://gdpr-info.eu/art-13-gdpr/)/[14](https://gdpr-info.eu/art-14-gdpr/)), and respect purpose and storage limitation ([Art. 5](https://gdpr-info.eu/art-5-gdpr/)).
   - Erasure requests ([Art. 17](https://gdpr-info.eu/art-17-gdpr/)) can only be executed by an admin device holding K. Pollis cannot locate a person's data inside ciphertext. Pollis can only delete a whole import on the controller's instruction, so the admin per-source erasure (`erase_import_source`, shared §8) is mandatory, not optional.
   - In Germany, introducing a messaging system and importing communications is subject to works-council co-determination under BetrVG §87(1) no. 6, because the system is objectively suited to monitoring behaviour ([CMS](https://cms.law/en/deu/legal-updates/it-mitbestimmung-und-ki); BAG 8 March 2022, 1 ABR 20/21, [summary](https://www.hensche.de/bag-beschluss-vom-08.03.2022-1-abr-20-21-zustaendigkeit-des-gesamtbetriebsrats-bei-unternehmenseinheitlicher-nutzung-von-microsoft-office-365.html)).
   - Where private use of Slack was permitted, accessing DMs risks telecommunications secrecy under §3 TDDDG. That point is contested; the NRW DPA's 2024 report says §3 does not bind employers but access still needs individual consent ([Computerwoche](https://www.computerwoche.de/article/2822162/was-firmen-duerfen-und-was-nicht.html)); see also [HSF](https://www.hsfkramer.com/notes/data/2021-11/new-telecommunications-telemedia-data-protection-act-ttdsg-comes-into-effect-on-1-december-2021) on criminal exposure under StGB §206.
   - Slack itself conditions full exports on member consent, legal process or a legal right ([guide](https://slack.com/help/articles/204897248-Guide-to-Slack-import-and-export-tools)). Participant-run DM import (Phase C) sidesteps the whole DM question, because each person moves only what they already have.
   - The import page must show a controller checklist: lawful basis, notice, works council where one exists, retention bound. It must default "Slack Connect channels" and "deactivated users" to off.
6. **Rate-limit math, 5 years, 200 people.** Assume 25 messages/person/workday, 250 workdays/year: 6.25M messages, of which 25% are thread replies in about 400k threads (assumptions, not Slack data).

   | Route | `conversations.history` | `conversations.replies` | Total |
   |---|---|---|---|
   | Export ZIP | 0 calls | 0 calls | Bound by file download bandwidth; `users.list` is 1-2 calls |
   | Customer internal app (Tier 3, 50/min, 1,000/page) | 4.69M / 1,000 = 4,690 calls, about 94 min | at least 400k calls (one per thread), about 8,000 min, **5.6 days** | about 6 days continuous, resumable |
   | Pollis-distributed unlisted app (1/min, 15/page) | 312,667 calls, **217 days** | at least 400k calls, **278 days** | **about 1.4 years**: infeasible, and prohibited by the Terms anyway |
   | Phase C, one participant's DMs (say 20k messages, 3k threads, internal app) | 20 calls | 3k calls, 60 min | about 1 hour |

   This is why the ZIP is primary, and why the API history path is only a fallback for workspaces that cannot get a full export (Pro) and only for conversations the token holder belongs to.
7. **Export file URL expiry.**
   - Links die when the owner revokes the export token, when retention deletes the file, or when the workspace closes.
   - On Free, file links are exported only for the last 90 days.
   - Apply downloads every file before publish and records per-file status (S4). The deletion checklist refuses to proceed while files are pending.
   - Anyone holding the ZIP can fetch every file. The report says to revoke the token, and Pollis never uploads the ZIP itself.
8. **Enterprise Grid, multiple workspaces.**
   - One group per workspace (#1179). Shared channels across workspaces of one org appear in several exports: dedupe on channel ID within one org-level import batch, and import a shared channel into exactly one group, chosen by the admin.
   - Identities key on the `W` ID, so the same person in five workspaces is one source identity per group but one DM with five links. That is Open decision 8.
   - Org-level retention and legal holds constrain Phase D, and the org owner, not the workspace owner, must act ([retention](https://slack.com/help/articles/203457187-Customize-message-and-file-retention)).
9. **Archive confidentiality, server-held encrypted history, redaction liveness and rollback** are shared concerns, **defined in the shared import spec** (shared §4.3 `Kpin` residual, §8 redaction and rollback, §16 principle text). Slack-specific consequence: personal archives (Phase C) survive device loss through account recovery; group archives survive only while some member device holds `Kpin`.
10. **Storage cost.** Five years of files can be hundreds of GB per customer, with no dedup (random keys by design). A quota per import is needed before GA (Open decision 7). An over-quota import should skip files, never messages.
11. **Brief corrections, consolidated.** Delivering K over MLS application messages is unnecessary (shared §4). Email-keyed reservations as a lookup are rejected (shared §5.4). `role` and `channel_grants` are dropped (shared §5.1). Slack exports do not normally include emails (Research §1). The API cannot read others' DMs at all (Research §2). Discovery API and MCP are unavailable or irrelevant (Research §3, §6). Pre-join readability already exists for pins (shared §16). The `POLLIS_DS_WATERMARK_STALE_MONTHS` 6-vs-12 point is not a discrepancy: the code fallback is 6 (`pollis-delivery/src/messages.rs:291`), and both deployed environments set 12 explicitly, as the comment above it says (`messages.rs:285-290`) and `.codesight/wiki/database.md:834` and `docs/deployments.md:187` document. CLAUDE.md's 12 is the configured value.

## Open decisions

1. Legal: does a Pollis client operating the customer's internal app, built from a Pollis-supplied manifest, fall under the API Terms' "provider" restrictions or the "templatized one-off custom apps" language? Get counsel before shipping the API history fallback and Phase C. The ZIP route for public content does not depend on the answer; Phase C does.
2. **Resolved (shared §5.5):** on the redeemer's account deletion the seat and claim are deleted, not kept as "claimed by a deleted account".
3. **Resolved (shared §7):** imported pins stay archive-only; no `pinned_message` rows.
4. **Resolved (shared §8):** the claimant rewrites their own leaves; admins apply erasure and file release.
5. **Resolved for v1 (shared §4.3):** no `Kpin` rotation on member removal; the wrap is CAS-updatable so a #99 follow-up needs no migration.
6. Large files above 104 MiB: skip in v1 (shared §3.4); chunked upload remains shared open decision 2.
7. Storage quota per import and per plan tier (shared §18).
8. Enterprise Grid: one link carrying several seats? (Shared I3 allows one seat per user per import, so this means one delivery message carrying several links, one per group.)
9. Slack Connect content: default off; offer opt-in at all?
10. Is Phase C in v1? Per CLAUDE.md "one ticket = one PR", if it is in #1179 it ships in #1179's PR. The schema already carries `scope = 'personal'` (shared §10).
11. **Resolved (shared §0):** #1179's ticket text is rescoped before code and its PR lands the shared scaffolding; no separate history ticket.
12. Slack-bound seats: accept DS-side verification of Slack OIDC ID tokens (revising #1179's "DS gains no Slack awareness"), or ship email binding only? v1 ships email only.
13. **Resolved (shared §5.5):** the link-creator cascade. Seats keep the redeemer on the seat row; teardown clears only the minter's dead links.
## Sources

Slack official (indexed copies; direct fetch was blocked from this environment):
- Export your workspace data: https://slack.com/help/articles/201658943-Export-your-workspace-data
- Guide to Slack import and export tools: https://slack.com/help/articles/204897248-Guide-to-Slack-import-and-export-tools
- How to read Slack data exports: https://slack.com/help/articles/220556107-How-to-read-Slack-data-exports
- Data management: https://slack.com/trust/data-management
- Usage limits for free workspaces: https://slack.com/intl/en-by/help/articles/115002422943
- Customize message and file retention: https://slack.com/help/articles/203457187-Customize-message-and-file-retention
- Legal holds: https://slack.com/help/articles/4401830811795
- Edit or delete messages: https://slack.com/help/articles/202395258-Edit-or-delete-messages
- Discovery API guide: https://slack.com/help/articles/360002079527-a-guide-to-slacks-discovery-apis
- Export and import workflows: https://slack.com/help/articles/30011815092499-Export-and-import-Slack-workflows
- Rate limit changes for non-Marketplace apps: https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps/ and https://api.slack.com/changelog/2025-05-terms-rate-limit-update-and-faq
- Rate limits: https://docs.slack.dev/apis/web-api/rate-limits
- conversations.history: https://docs.slack.dev/reference/methods/conversations.history/
- users.list: https://docs.slack.dev/reference/methods/users.list/
- chat.delete: https://docs.slack.dev/reference/methods/chat.delete/
- chat.postMessage: https://docs.slack.dev/reference/methods/chat.postMessage/
- Sign in with Slack: https://docs.slack.dev/authentication/sign-in-with-slack/
- Using PKCE: https://docs.slack.dev/authentication/using-pkce and https://docs.slack.dev/changelog/2026/03/30/pkce
- Enterprise development: https://docs.slack.dev/enterprise/developing-for-enterprise-orgs, https://api.slack.com/types/user, https://docs.slack.dev/reference/methods/migration.exchange
- Enterprise Key Management: https://api.slack.com/enterprise-key-management
- Slack API Terms of Service: https://slack.com/terms-of-service/api

Secondary:
- heise, free-plan deletion: https://heise.de/en/news/Slack-deletes-data-from-free-workspaces-after-one-year-9783229.html
- Forwarded Slack notice (27 June 2024): https://lists.osgeo.org/pipermail/uk/2024-July/001281.html
- Computerworld, API terms: https://www.computerworld.com/article/4005509/salesforce-changes-slack-api-terms-to-block-bulk-data-access-for-llms.html
- Hunton, API terms: https://www.hunton.com/insights/legal/salesforce-locks-down-slack-data-time-to-review-your-slack-api-terms
- Zulip Slack import: https://zulip.ipd.kit.edu/help/import-from-slack
- Mattermost forum, missing emails: https://forum.mattermost.com/t/bulk-import-from-slack-fails-with-message-error-bulkimport-some-users-not-found/17656
- Roam, missing emails: https://ro.am/support/slack-import-missing-emails
- slackdump export token: https://github.com/kolsys/slackdump/blob/master/doc/usage-export.rst
- DISCO, export token revocation: https://support.csdisco.com/hc/en-us/articles/18024077825933-Ingesting-Slack-exports-directly-into-Ediscovery
- Rate clamp observation: https://www.allanninal.dev/slack/non-marketplace-history-clamp/
- Method tiers: https://www.resumelens.org/blog/slack/slack-rate-limits-and-best-practices
- Slack MCP: https://github.com/hivegate-ai/hivegate/issues/96, https://www.truefoundry.com/blog/slack-mcp-server
- Logikcull legal holds: https://www.logikcull.com/blog/slack-legal-hold
- usehall export scope: https://usehall.com/guides/how-to-export-slack-conversations
- GDPR articles: https://gdpr-info.eu/art-5-gdpr/, https://gdpr-info.eu/art-6-gdpr/, https://gdpr-info.eu/art-13-gdpr/, https://gdpr-info.eu/art-14-gdpr/, https://gdpr-info.eu/art-17-gdpr/, https://gdpr-info.eu/art-28-gdpr/
- BetrVG §87(1) no. 6: https://cms.law/en/deu/legal-updates/it-mitbestimmung-und-ki, https://www.hensche.de/bag-beschluss-vom-08.03.2022-1-abr-20-21-zustaendigkeit-des-gesamtbetriebsrats-bei-unternehmenseinheitlicher-nutzung-von-microsoft-office-365.html
- TDDDG §3: https://www.computerwoche.de/article/2822162/was-firmen-duerfen-und-was-nicht.html, https://www.hsfkramer.com/notes/data/2021-11/new-telecommunications-telemedia-data-protection-act-ttdsg-comes-into-effect-on-1-december-2021

Repo: `pollis-schema/migrations/000000_baseline.sql`, `000012_attachment_ref.sql`, `000014_group_invite_link.sql`, `000019_pinned_messages.sql`, `000020_vault_message.sql`, `000026_email_case_insensitive.sql`; `pollis-delivery/src/groups.rs` (redeem), `messages.rs` (`live_ref_exists!`, retention), `broker.rs` (`R2Family`); `pollis-api/src/lib.rs` (`endpoints!`), `groups.rs`, `broker.rs`; `pollis-core/src/commands/pinned_messages.rs`, `r2.rs`, `export.rs`, `groups/invite_token.rs`, `db/local_schema.sql`; `.codesight/wiki/mls.md`; `docs/security-whitepaper.md`; `docs/backend-core-invariants.md`.

## Review notes

Adversarial review against `main` at `ec22dec`. Items about shared scaffolding were then carried into `docs/import-history-design.md`; such items say **Resolved (shared §N)**. The schema, endpoint and archive-format details in the items below describe this file's earlier draft and are superseded by the shared spec. Changes made:

- Corrected: reservation redeemers join by external commit, not Welcome; `try_adopt_pin_key` does not run for them and they wait on #99's "not yet available" state (`pollis-core/src/commands/groups/invites.rs:333-339,397-410`; `.codesight/wiki/mls.md:649-652`). **Resolved (shared §4.2).**
- Corrected: the "all channels share one MLS group" citation pointed at the pin section; it is `.codesight/wiki/mls.md:3` and `000019_pinned_messages.sql:3-4`. `channel_type` text|voice confirmed (`frontend/src/types/index.ts:63`, `mobile/types/index.ts:35`); baseline column has no CHECK (`000000_baseline.sql:20`); no channel ACL table found.
- Changed: K is wrapped under a `Kpin`-derived subkey with AAD, not raw `Kpin`; apply must call `ensure_kpin` with mint because create-time mint is best-effort (`pinned_messages.rs:452-463`). **Resolved (shared §2, §4.1);** label reconciled to `pollis/import-wrap/v1 || import_id`; `ensure_kpin` exists at `pinned_messages.rs:258` and must become `pub(crate)`.
- Added: Kpin's own loss mode now also loses imports (`pinned_messages.rs:43-46`). **Resolved (shared §4.3)**, no extra K escrow.
- Verified: no pins-disabled setting exists, so that failure mode does not arise.
- Marked unsound: the Slack-bound seat. v1 is email binding only; DS-side OIDC verification is Open decision 12. `source_account_mac` is not in the migration (shared §5.1).
- Fixed inconsistency: the identity salt. **Resolved (shared §2):** `source_ref` is an HMAC under `HKDF(K, "pollis/import-source-ref/v1")`; no salt column, no pre-join client check.
- Added: claim signature domain separation (old I18). **Resolved (shared §2, §6, I9).**
- Schema: composite FKs, the trigger for single-use expiring same-group links, invalid states admitted. **Resolved (shared §10):** no FK is relied on (production runs `foreign_keys=OFF`, `teardown.rs:8`, so this draft's cascades were inert); every rule is a CHECK, UNIQUE or trigger (shared I1, I13-I15, I25, I27).
- Added: cascade hazard of `group_invite_link.created_by` (`000014_group_invite_link.sql:42`). **Resolved (shared §5.5, I10):** the real deleter is `purge_user_rows` (`teardown.rs:299-306`); seats keep their redeemer and claims are keyed by seat.
- Corrected: read endpoints renamed to the `/v1/read/...` convention; realtime hint targets. **Resolved (shared §12).**
- Corrected: `https://pollis.com/invite/#<token>` does not work today. **Resolved (shared §5.2):** seat links use `pollis://invite/<token>` plus the bare token; no fragment form, so no `extract()` change is needed and the oldest store build parses them.
- Corrected: the watermark 6-vs-12 "discrepancy" is documented and intended (`pollis-delivery/src/messages.rs:285-291`, `.codesight/wiki/database.md:834`).
- Verified: `export.rs:22-24` contains the quoted #856 line. I14 blocks only the naive route. **Resolved (shared §3.5, §16, I24).**
- Verified: migration 000035 is the latest, so 000036 is next on `main`.
- Verified: attachments are convergent and GC'd through the derived ref count (`000012_attachment_ref.sql:3-5`, `r2.rs:755`, `messages.rs:1913`); `R2_MEDIA_MAX_BYTES` 104 MiB (`pollis-api/src/broker.rs:99`); `R2Family` (`pollis-delivery/src/broker.rs:1431`); internally tagged `RedeemInviteLinkResponse` (`pollis-api/src/groups.rs:253`); no `deny_unknown_fields`.
- Removed the redeem-request `source_account` field from v1.
- Missed by this draft, found by `docs/discord-import-design.md`: the redeem `is_member` short-circuit (`pollis-delivery/src/groups.rs:1687-1695`) meant an existing member could never claim. **Resolved (shared §5.3, I6).**
- Missed by this draft: Maestro flow names must be kebab-case (`mobile/.maestro/flows/`). **Resolved (shared §15):** `import-claim.yaml`, `import-history.yaml`, `import-redact.yaml`, `import-seats.yaml`.
- External claims checked: the 2025 rate clamp (1/min, 15 objects; new installs 29 May 2025; existing unlisted installs 3 March 2026) is confirmed by search snippets of docs.slack.dev/api.slack.com changelogs. The "limit default 1,000" is marked unverified.
- External: added Slack's "templatized one-off custom apps" Marketplace language (https://docs.slack.dev/changelog/2025/05/29/tos-updates) and the Dec 2024 dev policy (https://docs.slack.dev/changelog/2024/12/10/dev-policy-update). Both threaten the Pollis-supplied-manifest route and Phase C; Open decision 1 expanded.
- External: added that the Marketplace guidelines list export/backup apps as unsuitable (search snippet). The terms' "effective 10 October 2025" date is marked unverified.
- External: Slack MCP GA on 17 Feb 2026 is now cited to Slack's own blog (https://slack.com/blog/news/mcp-real-time-search-api-now-available). The endpoint, admin approval and tool list are marked secondary.
- External: Free export is likely only 90 days (https://slack.com/help/articles/29414264463635), not "at most the last year". PKCE GA on 30 March 2026 and the one-way, 30-day refresh behaviour are confirmed. "Desktop redirects cannot request bot scopes" is marked unverified.
- External: email absence from `users.json` rests on secondary sources only (Roam, Zulip, Mattermost). No Slack page confirming the Email Display behaviour was found; left as is.
- CLAUDE.md: the mobile exemption is narrowed to the long-running job. **Resolved (shared §15, UI above).**
- Docs list extended. **Resolved (shared §16).**
- Added email-binding UX cost and the account-to-workspace linkage the DS learns at redemption (Pollis changes §2).
- Added a redaction rollback concern. **Resolved as a documented residual (shared §8).**

Unresolved concerns:

- Legal: whether the customer-installed, Pollis-templated app counts as distributed decides Phase C and the API fallback. Only counsel can settle it (Open decision 1). **Still open.**
- An unbound seat lets a leaked-link holder claim another person's source identity. **Resolved for v1:** seats are email-bound by default wherever an email exists (Phase A step 6); unbound seats are shown as such.
- Kpin never rotates. **Resolved for v1 as a documented residual (shared §4.3);** rotation is a #99 follow-up.
- The fix for the link-creator cascade. **Resolved (shared §5.5, I10).**
- I3 forbids one person claiming two source identities. **Resolved:** intended for v1; override is shared open decision 4.
- `import_annotation` storage abuse. **Resolved (shared §10, I26):** 4 KiB per annotation, 1000 per author per import, current members only.
- Nothing pins the manifest. **Resolved (shared §3.3):** write-once, content-addressed key, frozen by trigger, hash pinned by clients.
- Personal archives inherit the vault's `reset_identity` loss. **Resolved (shared §4.4):** the reset flow warns.
- Rate-limit arithmetic in "Where this breaks" 6 was rechecked and is internally consistent. Its inputs are assumptions, not Slack data.
- The GDPR, BetrVG and TDDDG citations were not independently verified in this review. **Still open.**
