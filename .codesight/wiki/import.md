# Importing from Discord and Slack

Bringing a community over from another chat platform. Discord first (#1180): its
**structure** (categories, channels in order, topics, voice channels, custom emoji, the
role collapse, an invite plan), read from an export made on the user's own machine.
Message history is not imported (see "History: design, not built"). Slack (#1179) reuses
the same pipeline.

## Pipeline

```
export folder ──read──▶ model::Export ──plan──▶ plan::ImportPlan ──apply──▶ device-signed DS calls
 (pollis-import: pure, no network, no keys)                     (pollis-core::commands::import)
```

- **Read** (`pollis-import/src/discord/`): every DiscordChatExporter (DCE) `*.json` in one
  folder (`dce.rs`), then a guild snapshot beside them if there is one (`snapshot.rs`).
  Wire structs are lenient (every field defaulted, unknown fields ignored) so exports
  from any DCE version parse; a bad file is a warning, never a failed import. The server
  is chosen by **majority of server-channel files**, never by file order (DCE files DM
  exports under a pseudo-guild that sorts first).
- **Model** (`model.rs`): source-neutral. Holds what the source said; renaming and
  dropping are the plan's decisions, so the plan can report them.
- **Plan** (`plan.rs`): deterministic (same export, same target state, same plan). Its
  options describe what the target group already holds, and everything already there is
  marked `exists`; that is what makes a re-run idempotent. Everything it says to a person
  is structured (`Note`, `ChannelNote`, `EmojiNote`, `SkipReason`, `Permission`) so each
  app renders it in the reader's language.
- **Report** (`report.rs`): English text for the CLI, and the invite sheet (CSV, cells
  quoted and formula characters defused). The desktop page renders the plan natively.
- **Apply** (`pollis-core/src/commands/import.rs`): `discord_import_preview` and
  `discord_import_apply`. See "Apply" below.
- **CLI**: `cargo run -p pollis-import -- plan <export-dir> [--json | --sheet]`. Read-only.

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

## Apply

`pollis-core/src/commands/import.rs`, every write an existing device-signed command; the
DS gains no Discord awareness.

- **Preview** (`discord_import_preview(dir, user_id, target?)`): reads and plans; writes
  nothing. `target` omitted picks the one admin-held group whose name matches the server's
  (a re-run) or else a new group; `"new"` forces a new group; a group id targets it
  (admins only). Returns the plan, the invite sheet, the target and the candidates.
- **Apply** (`discord_import_apply`): re-plans against the target (a plan from the renderer
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

## Desktop page

`/groups/import` (`pages/DiscordImportPage.tsx`, `components/Import/`), reached from the
Groups menu, Cmd+K ("Import from Discord") and the breadcrumb. A page, never a dialog:
choose the folder, read the dry run (channels by category with every dropped permission,
emoji, roles, people, notes, parse warnings), pick the target in a radio list when a
matching group exists, Import. The result shows the counts, any failures, the new invite
link (`CreatedInviteLinkCard`, shown once) and a way to the group. Copy lives in the
`import` i18n namespace.

**Mobile: not built, deliberately.** The export is a folder on a computer (DCE is a
desktop program); a phone has no such folder and no way to pick one. The group an import
makes is an ordinary group, so mobile shows its categories, order and emoji like any
other. `scripts/feature-parity.json` records the exemption.

## Tests

- `pollis-import/tests/plan_fixture.rs` over two fixtures: `tests/fixtures/dnd` (DCE files
  only: every lossy path of a channel export) and `tests/fixtures/dragons-den` (the same
  server with a snapshot: positions, an empty category, voice and stage, a forum, roles
  with permissions, overwrites including one private channel, a member who never wrote,
  emoji images, an icon; regenerate with `tests/fixtures/gen_dragons_den.py`).
- `src-tauri/tests/flows/discord_import.rs`: the real commands against the in-process DS
  (order, a second run creating nothing, a partial run repaired in place, a member refused,
  the shim refusing an unpicked folder). That DS has no object storage, so emoji and the
  icon are expected failures there.
- `pollis-tui/examples/discord_import_peer.rs`: the same calls against the dev DS, where
  the emoji and icon uploads are real.
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
- Slack (#1179).
