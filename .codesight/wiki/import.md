# Importing from Discord and Slack

Bringing a community over from another chat platform. Discord first (#1180), from a
DiscordChatExporter (DCE) export; Slack (#1179) reuses the same pipeline.

## Pipeline

```
export on disk ──parse──▶ model::Export ──plan──▶ plan::ImportPlan ──apply──▶ device-signed DS calls
  (pollis-import: pure, no network, no keys)          (pollis-core: not built yet)
```

- **Parse** (`pollis-import/src/discord/dce.rs`): reads every `*.json` in one folder.
  Wire structs are lenient (every field defaulted, unknown fields ignored) so exports
  from any DCE version parse. A bad file is a warning, never a failed import. The
  server is chosen by **majority of server-channel files**, never by file order: DCE
  files DM exports under a pseudo-guild ("Direct Messages") that sorts first.
- **Model** (`model.rs`): source-neutral. Holds what the source said; renaming and
  dropping are the plan's decisions, so the plan can report them.
- **Plan** (`plan.rs`): deterministic (same export, same options, same plan), which is
  what makes a re-run idempotent. Measures every channel's history even when history is
  not imported, so the admin sees what stays behind.
- **Report** (`report.rs`): plain text for the CLI. The apps render the plan natively.
- **CLI**: `cargo run -p pollis-import -- plan <export-dir> [--json] [--drop-categories]`.
  Read-only; safe to point at a real export today.

## Making the export

The guild owner runs DiscordChatExporter with a **bot token** (a user token is a
self-bot, against Discord's terms), `--format Json`, and **`--media`** so attachments,
avatars and emoji are downloaded beside the JSON. Discord's CDN links are signed and
expire, so an export without `--media` keeps dead URLs; the plan counts those as
`remote_only` and says to re-export. Include threads (DCE's thread option) or they are
missing; a thread whose parent channel was not exported is reported, not guessed at.

## Mapping decisions (implemented)

| Discord | Pollis | Rule |
|---|---|---|
| Guild | group | name fitted to `pollis_api::profile::is_valid_display_name` |
| Text, announcement, forum | text channel | announcement and forum noted in the report |
| Voice, stage | voice channel | |
| Category | name prefix `Category / channel` (default) or dropped (`--drop-categories`) | Discord's default "Text Channels" / "Voice Channels" never prefix |
| Thread | Pollis thread under its starter message | a thread shares its starter's id; a thread without one (forum post, deleted starter) is folded in with a heading |
| DM / group DM | not imported | reported |
| System notices (joins, pins, renames, calls) | not imported | counted |
| Embeds (link previews) | not imported | counted; Pollis renders no generic previews by design |
| Custom emoji | group emoji | collision policy below; only emoji seen in the export (DCE has no emoji list) |
| Roles | none | the importer owns the group; everyone else joins as a member |
| Members | invite plan | no emails exist in Discord; the plan lists people (bots separately) for the invite link |
| Channel order | not kept | `channels` has no position column |

**Shortcode policy** (`shortcode.rs`, shared by every source): lower-case, map other
characters to `_`, collapse and trim `_`, cut to 32; under 2 characters is skipped and
reported; a taken name (earlier in the import, or already in the group) gets the first
free `_2`, `_3`, ... within 32. Most-used emoji are fitted first, so the popular one keeps
the plain name. `shortcode_is_valid` and `is_valid_display_name` now live in `pollis-api`,
the one definition the DS, the apps and the importer share.

## History: design, not built

#1180 as filed imports **structure only**. A community with years of history (the
motivating case: a D&D group, ~8 people, several years of messages and images) wants the
history too. What constrains it:

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
("History imported from Discord, 2019-09 to 2024-11") that references the chunks (so the
attachment references bind to a real envelope and survive GC) and pins it (so late
joiners discover it through `Kpin`). Each member's client, on first opening the channel,
fetches the chunks and materialises them into its local DB as `message` rows marked
imported (shown with a Discord badge and the source author's name, kept out of unread
counts and notifications, indexed for search). A client that predates this sees a pinned
message with attachments, which degrades honestly.

Open: whether to accept imported history being readable by later joiners (the Discord
behaviour; the product's default is the opposite for native messages), how imported
authors map to Pollis accounts (cosmetic only), and storage bounds for multi-GB imports.

## Not built yet

- The apply step in `pollis-core` (group, channels, emoji via `upload_group_emoji`,
  history per the decision above), its progress UI, and the desktop and mobile pages.
- Reading the bot API directly (the #1180 ticket's original path) instead of a DCE
  export.
- Slack (#1179).
