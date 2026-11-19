# Importing from Discord and Slack

Bringing a community over from another chat platform: its **structure**, read from an
export the user made, never its messages (see "History: design, not built"). Two sources,
one pipeline:

- **Discord** (#1180): categories, channels in order, topics, voice channels, custom
  emoji, the role collapse and an invite plan, from a DiscordChatExporter export.
- **Slack** (#1179): channels in Slack's order with purpose and topic, custom emoji, an
  invite plan with email invites, user groups listed, from the workspace export an admin
  downloads. See "Slack" below.

## Pipeline

```
export folder ──read──▶ model::Export ──plan──▶ plan::ImportPlan ──apply──▶ device-signed DS calls
 (pollis-import: pure, no network, no keys)                     (pollis-core::commands::import)
```

- **Read**: one reader per source, each producing the same `model::Export`
  (`pollis_import::read_export(SourceKind, path)`). Readers only read; they never plan.
  Discord (`pollis-import/src/discord/`): every DiscordChatExporter (DCE) `*.json` in one
  folder (`dce.rs`), then a guild snapshot beside them if there is one (`snapshot.rs`).
  Wire structs are lenient (every field defaulted, unknown fields ignored) so exports
  from any DCE version parse; a bad file is a warning, never a failed import. The server
  is chosen by **majority of server-channel files**, never by file order (DCE files DM
  exports under a pseudo-guild that sorts first). Slack (`pollis-import/src/slack/`): see
  "Slack" below.
- **Model** (`model.rs`): source-neutral. Holds what the source said; renaming and
  dropping are the plan's decisions, so the plan can report them.
- **Plan** (`plan.rs`): deterministic (same export, same target state, same plan). Its
  options describe what the target group already holds, and everything already there is
  marked `exists`; that is what makes a re-run idempotent. Everything it says to a person
  is structured (`Note`, `ChannelNote`, `EmojiNote`, `SkipReason`, `Permission`) so each
  app renders it in the reader's language.
- **Report** (`report.rs`): English text for the CLI, and the invite sheet (CSV, cells
  quoted and formula characters defused). The desktop page renders the plan natively.
- **Apply** (`pollis-core/src/commands/import.rs`): `import_preview` and `import_apply`,
  taking the source (`"discord"` / `"slack"`). Everything after the read is source-neutral.
  See "Apply" below.
- **CLI**: `cargo run -p pollis-import -- plan [--slack] <export> [--json | --sheet]
  [--include-archived]` (a `.zip` implies `--slack`). Read-only.

## Input

**A DCE export is the primary path.** The guild owner runs DiscordChatExporter on their
own machine with a **bot token** (a user token is a self-bot, against Discord's terms),
`--format Json`, and **`--media`** so attachments, avatars and emoji are downloaded beside
the JSON (Discord's CDN links are signed and expire). Pollis never holds a Discord
credential.

A DCE export is per channel. It carries no channel positions, no empty categories, no
role permissions, no per-channel permission overwrites and no emoji list (only emoji
someone used). Newer DCE versions record each author's roles (name, position) but not
their permissions.

**The guild snapshot** fills that gap: `discord-guild.json` in the same folder holds three
Discord REST responses verbatim, plus optional images:

```
discord-guild.json  { "guild":    GET /guilds/{id}           (name, owner_id, roles, emojis)
                      "channels": GET /guilds/{id}/channels  (type, position, parent_id, topic, permission_overwrites)
                      "members":  GET /guilds/{id}/members   (optional; needs the GUILD_MEMBERS intent) }
emoji/<emoji id>.png|gif|webp   (optional)
icon.png|jpg|gif|webp           (optional)
```

The snapshot decides structure and the channel files supply the history measurement; a
snapshot alone is a complete structure import. It is exactly what a direct bot-token
reader would write (not built: see "Not built yet"), so that reader only fetches; it never
plans. Today a snapshot is made by hand from those three endpoints with the same bot
token DCE used.

## Mapping (implemented)

| Discord | Pollis | Rule |
|---|---|---|
| Guild | group | name fitted to `pollis_api::profile::is_valid_display_name`; icon re-encoded like a profile picture into `group-icons/<group>/<sha>` (new group only) |
| Category | channel category (#2031) | in Discord's order (position, then creation); every category, empty ones too with a snapshot; names fitted like group names, duplicates suffixed ` (2)` |
| Text, announcement, forum, media | text channel | announcement and forum noted; topic → description |
| Voice, stage | voice channel | stage noted |
| Channel order | kept | uncategorised block first, then each category's; text-like before voice-like (as Discord draws a category), each by position, then creation. Without a snapshot, creation order |
| Thread | not imported | history only; counted under its parent |
| DM / group DM | not imported | reported |
| Custom emoji | group emoji | collision policy below; image through the shared `upload_group_emoji` re-encode (48 KiB, WebP or GIF) |
| Roles | member / admin | `ADMINISTRATOR` or `MANAGE_GUILD` → admin; `@everyone` → member; a bot's managed role and every other role are dropped and listed with how many hold them. Without a snapshot no permission is known, so nobody maps to admin |
| Per-channel overwrites | none | every one listed under its channel (target, allowed and denied permissions); a channel whose `@everyone` overwrite denies `VIEW_CHANNEL` is flagged **private on Discord, visible to every Pollis member** |
| Members | an invite plan | Discord exposes no emails, so no invite is sent: one invite link plus the sheet (username, display name, admin or member, Discord roles, channels they wrote in) |

**Shortcode policy** (`shortcode.rs`, shared by every source): lower-case, map other
characters to `_`, collapse and trim `_`, cut to 32; under 2 characters is skipped and
reported; a taken name (earlier in the import, or already in the group) gets the first
free `_2`, `_3`, ... within 32. Most-used emoji are fitted first, so the popular one keeps
the plain name. `shortcode_is_valid` and `is_valid_display_name` live in `pollis-api`.

## Slack

**The workspace export is the primary path.** A workspace admin downloads it from Slack's
admin pages (Settings, Import/Export Data, Export) as a ZIP; no token and no app install.
The page takes the ZIP as downloaded or the folder it unpacks to (`slack/archive.rs`:
reads are bounded, ZIP entry names are only ever looked up, never joined onto a path we
write; images inside a ZIP are copied to a temp cache under hashed names for the upload).

```
channels.json        public channels (name, purpose, topic, is_archived, is_general, members)
groups.json          private channels (exports that include them)
users.json           name, profile.display_name/real_name/email, is_admin/is_owner, deleted, is_bot
dms.json, mpims.json reported as not imported
<channel>/<day>.json measured only (message counts, dates, emoji uses), never imported
```

A standard export has no custom emoji, no user groups and no workspace name, so the
**workspace snapshot** sits beside it (in the folder or the ZIP):

```
slack-workspace.json { "team": team.info → team, "emoji": emoji.list → emoji,
                       "usergroups": usergroups.list → usergroups (include_users) }
emoji/<name>.png|gif|jpg|jpeg|webp   (optional)
icon.png|jpg|gif|webp                (optional)
```

Without it the group is named from the download's file name ("Northwind Studio Slack
export Jan 1 2021 - Nov 18 2026.zip" → "Northwind Studio") and the plan says so. Underscores
count as spaces and a leading 8-hex-digit `xxxxxxxx-` upload prefix is dropped
("79e62724-Northwind_Studio_Slack_export_….zip" → "Northwind Studio"); a name without
" Slack export " is used whole. `users.json` carries the team's id but never its name.

| Slack | Pollis | Rule |
|---|---|---|
| Workspace | one group | name from the snapshot, else the file name; icon from the snapshot |
| Channel | text channel | no categories; Slack's sidebar order: the default channel (`is_general`) first, then alphabetical |
| Purpose, topic | description | purpose, then the topic on a second line when it differs |
| `is_private` / `groups.json` | channel, flagged | **private on Slack, visible to every Pollis member** |
| `is_archived` | skipped by default | listed with the reason; `include_archived` brings them, marked |
| DM, group DM | not imported | listed |
| Custom emoji | group emoji | the shared shortcode policy (`party-parrot` and `party_parrot` → `:party_parrot:` and `:party_parrot_2:`, the more used one keeping the plain name; at equal use, a name that is already valid keeps it) |
| `alias:other` | its own shortcode for the target's picture | an alias of a standard emoji, or of one not imported, is skipped and reported |
| Admins, owners | "make admin" on the sheet | the primary owner is the owner |
| Members | an invite plan | per-person roster from channel `members`; emails on the sheet; with `invite_by_email`, an ordinary group invite to every email that belongs to a Pollis account |
| Deactivated accounts | left out | never invited, dropped from user groups |
| Bots, apps, Slackbot | listed apart | never invited |
| User groups | listed only | no mention tags yet (#294) |

**Email invites** go through `groups::invite_to_group` (the core of `send_group_invite`),
one per planned email; the DS resolves the email. An email with no account (or one the
DS declines to resolve) is "no account": that person joins through the invite link,
listed by name on the result. A re-run answers "already invited" or "already a member",
so nobody is invited twice. The admin's own email answers "you". Nothing is invited
unless the admin leaves the page's switch on, and nobody joins until they accept.

## Apply

`pollis-core/src/commands/import.rs`, every write an existing device-signed command; the
DS gains no Discord awareness.

- **Preview** (`import_preview(source, path, user_id, target?, include_archived?)`): reads and plans; writes
  nothing. `target` omitted picks the one admin-held group whose name matches the server's
  (a re-run) or else a new group; `"new"` forces a new group; a group id targets it
  (admins only). Returns the plan, the invite sheet, the target and the candidates.
- **Apply** (`import_apply(source, path, user_id, target?, { include_archived, invite_by_email })`): re-plans against the target (a plan from the renderer
  is never trusted), then: `create_group` with no default channels (+ icon); categories
  not already there (matched by name, case-insensitive); channels not already there
  (matched by category name + channel name), each at the bottom of its block, so creating
  in plan order keeps the order with no moves; after a partial earlier run, `move_channel`
  puts the planned channels back in plan order; emoji not already there; one invite link
  (7 days, unlimited uses) unless the group has a live one.
- **Idempotent.** Emoji are matched **by picture**, not by name: each source image is
  re-encoded exactly as an upload would be and its content hash compared with the group's,
  so a re-run neither uploads a copy nor renames `nat20` to `nat20_3`. A second run creates
  nothing (flows test and the dev-DS check below).
- **Failures** are per item (`ImportFailure { kind, name, error }`); the rest goes ahead
  and a re-run retries them.
- The desktop shim requires the folder to have been chosen in this device's picker
  (`src-tauri/src/pathscope.rs`).

## Desktop pages

`/groups/import` (Discord) and `/groups/import/slack` (Slack): one page,
`pages/ImportPage.tsx` with a `source` prop, over `components/Import/` (`ImportPlanView`,
`ImportResultView`, both source-neutral). Reached from the Groups menu, Cmd+K ("Import from
Discord" / "Import from Slack") and the breadcrumb. A page, never a dialog: choose the
export (Discord: a folder; Slack: the ZIP, or the unpacked folder), read the dry run
(channels with descriptions and every dropped permission or privacy, what is not imported
and why, emoji, roles, user groups, people with their emails and rosters, notes, parse
warnings), pick the target in a radio list when a matching group exists, and for Slack two
switches: bring archived channels (re-reads the plan) and invite people who already use
Pollis (on by default, shown only when the export has emails). Import. The result shows
the counts, the invites (invited, already invited, already in, and who has no account
yet, by name), any failures, the new invite link (`CreatedInviteLinkCard`, shown once)
and a way to the group. Copy lives in the `import` i18n namespace: `sourceName`, a block
per source, and shared `page`, `plan` and `result` blocks.

**Mobile: not built, deliberately, for either source.** The export is a file made on a
computer: DCE is a desktop program, and Slack's workspace export is downloaded by an admin
from Slack's admin pages in a browser. A phone has neither to hand, and migrating a
workspace is an admin task at a desk. The group an import makes, its emoji and its
invites are ordinary, so mobile shows and accepts them like any other.
`scripts/feature-parity.json` records the exemption.

## Tests

- `pollis-import/tests/plan_fixture.rs` over two fixtures: `tests/fixtures/dnd` (DCE files
  only: every lossy path of a channel export) and `tests/fixtures/dragons-den` (the same
  server with a snapshot: positions, an empty category, voice and stage, a forum, roles
  with permissions, overwrites including one private channel, a member who never wrote,
  emoji images, an icon; regenerate with `tests/fixtures/gen_dragons_den.py`).
- `pollis-import/tests/slack_fixture.rs` over `tests/fixtures/northwind`, a Slack
  workspace export in Slack's own shapes with a workspace snapshot (regenerate with
  `tests/fixtures/gen_northwind.py`): public, private and archived channels, purpose and
  topic, users with and without email, admins, a guest, a deactivated account, bots and
  Slackbot, DMs, emoji colliding after normalising, aliases (to a custom emoji and to a
  standard one), user groups. Also read as a ZIP (flat and wrapped in a folder, with an
  entry that tries to climb out) and without its snapshot.
- `src-tauri/tests/flows/slack_import.rs`: Slack through the real commands and the
  in-process DS: order and descriptions, a re-run creating nothing, archived channels only
  when asked (slotting into place), and email invites reaching only the account that
  exists, once (then "already invited", then "already a member").
- `src-tauri/tests/flows/discord_import.rs`: the real commands against the in-process DS
  (order, a second run creating nothing, a partial run repaired in place, a member refused,
  the shim refusing an unpicked folder). That DS has no object storage, so emoji and the
  icon are expected failures there.
- `pollis-tui/examples/import_peer.rs` (`discord|slack <email> <export> [--invite-by-email]`,
  or `signup <email>` for an invitee; `@pollis.test` addresses only): the same calls against the dev DS, where
  the emoji and icon uploads are real.
- `e2e/slack-import.spec.ts`: the Slack page in both skins over
  `e2e/fixtures/slack-import-preview.json`. Both preview fixtures are real `pollis-import`
  output, regenerated by `e2e/fixtures/gen_import_previews.py`.
- `e2e/discord-import.spec.ts`: the page in both skins over the real plan of the fixture
  (`e2e/fixtures/discord-import-preview.json`, from `pollis-import plan --json`).

## History: design, not built

Not imported, and not to be built until a decision is made. A community with years of
history (the motivating case: a D&D group, ~8 people, several years of messages and
images) wants it. What constrains it:

1. **MLS loss (1)**: a message sealed now reaches only the members in the tree now. A
   history replayed as ordinary messages would be invisible to everyone who joins
   afterwards, and in a migration nearly everyone joins afterwards.
2. **The file key ring is readable by late joiners, deliberately** (#1382, `commands.md`
   "file_keys"), and so are **pins** (`Kpin`, #99). Both are the existing, reviewed
   exceptions to (1).
3. **Attribution cannot be cryptographic.** The importer's device signs everything it
   uploads; "written by Ash on 2021-03-05" is the importer's claim.

**Proposed shape (pending decision):** per channel, the importer packs its history into
compressed JSON chunks, uploads each chunk and every downloaded image as ordinary
attachments sealed under the channel's file key ring, and posts one message per channel
("History imported from Discord, 2019-09 to 2024-11") that references the chunks and pins
it (so late joiners discover it through `Kpin`). Each member's client, on first opening the
channel, fetches the chunks and materialises them into its local DB as `message` rows
marked imported (a Discord badge and the source author's name, kept out of unread counts
and notifications, indexed for search).

Open: whether imported history may be readable by members who join after the import (the
Discord behaviour; the product's default is the opposite for native messages), how
imported authors map to Pollis accounts (cosmetic only), and storage bounds for multi-GB
imports.

## Not built yet

- **History**, per the decision above.
- **A direct bot-token reader** (the ticket's original path): fetch the three snapshot
  endpoints plus emoji images from the CDN, honouring 429 `retry-after`, and write the
  snapshot folder. Small, but it needs a Discord application and a test guild to verify,
  and it would have the app hold a Discord bot token.
- **Admin promotion after people join**: the sheet says who to make admin; nothing
  matches a joining Pollis account to a Discord username.
- **Group icons are stored but not drawn**: `groups.icon_url` is set, and no screen shows
  a group icon yet.
- **A direct Slack reader** (OAuth token: `channels:read`, `groups:read`, `users:read`,
  `users:read.email`, `emoji:read`, `usergroups:read`, held in memory on the device, calls
  straight to `slack.com/api/*`): it would write the same export files and
  `slack-workspace.json` and never plan. Not built: it needs a Slack app and a test
  workspace to verify, and many workspaces require an admin to approve app installs.
