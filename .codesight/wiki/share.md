# Sharing into Pollis (#2100)

Text, links, photos, videos and files from another app's system share sheet go to up to five
Pollis chats (DMs, text channels and the Vault). The share sheet shows **Pollis, the app**, and
never a person, group or channel: the chat is chosen inside Pollis, after an unlock.

## What the share sheet shows

- **iOS:** the `PollisShare` share extension declares **no** `IntentsSupported` /
  `INSendMessageIntent`, so iOS has nothing to suggest people or chats from (the Notification
  Service Extension's donated communication intents stay out of the share sheet). Its activation
  rule is the dictionary form (text, one web URL, up to 10 images, movies and files), never
  `TRUEPREDICATE`. Pinned by `mobile/tests/share-extension.test.ts`.
- **Android:** one activity, `ShareIntakeActivity`, answers `SEND` / `SEND_MULTIPLE` for
  `text/*`, `image/*`, `video/*`, `audio/*`, `application/*`. No Sharing Shortcuts
  (`<share-target>`), no `ChooserTargetService`, no published shortcuts. Disabled in the manifest
  and enabled only while someone is signed in (`PollisShareModule.setShareTargetEnabled`, turned
  off by the sign-out teardown), so a signed-out install is not offered. Pinned by
  `mobile/tests/android-share-intake.test.ts`.
- **Desktop:** exempt. A Tauri bundle cannot carry a macOS share extension, Windows offers share
  targets only to packaged (MSIX) apps, and Linux has none; drag-and-drop and paste already put
  files and text into a conversation there.

## pollis-core (`pollis-core/src/share/`)

- **`targets::share_targets(query, limit)`** — the one query behind both pickers. The Vault
  first, then accepted DMs whose other members the user has not blocked and the text channels of
  the user's groups, newest local activity first (as the DM list sorts, #1838), then DMs newest
  made, then by name. Never a DM request, a blocked DM or a voice channel. One DS round trip (the
  sidebar's bootstrap) plus one local read. A query matches word prefixes of every name
  (display name, username, channel, group). Bridge: `share_targets`.
- **`send::enqueue_share(request)` / `send::deliver_share(queued, progress)`** — one draft (an
  optional message, which a shared text or link pre-fills, and up to 10 files of ≤ 100 MB) to 1–5
  targets. Every target's row is stored before any is delivered: a chat's as an ordinary outbox
  message (#1335) sealed under its own MLS group with files under its own file key ring (#1382);
  the Vault's through the Vault's queue (below). Row ids are derived from the share's ULID and the
  conversation (`target_message_id`), so running the same share again adopts its rows and the DS
  answers a re-posted envelope id as a replay: nothing is sent twice. A target the user may not
  share to refuses the whole share before anything is stored. Every target's id is claimed
  (`outbox_claim`) until all are stored, so no drain delivers part of a share; one that fails
  part-way takes back the rows it wrote, so nothing of it goes out. A link preview is built for the
  first https link when previews are on (`link_preview`, #1987). Bridge: `share_enqueue`,
  `share_deliver`.
- **Streamed files.** An outbox attachment is stored into `outbox_attachment` with SQLite
  incremental blob I/O (a zero-filled blob of the file's size, filled 1 MiB at a time) and
  uploaded by `r2::StreamUpload`: read back 4 MiB at a time, hashed and sealed with the same
  chunked AES-256-GCM (byte-identical objects), the ciphertext written to `<data_dir>/upload-tmp/`
  and PUT from disk with an explicit Content-Length. Peak memory is a couple of chunks whatever
  the size, which is what lets the iOS extension send a 100 MB video. This is every outbox send's
  path now, not only shares. Images ≤ 24 MiB the sender did not measure are still decoded for
  their blurhash and size, one at a time per process, with a 128 MiB allocation limit, and a
  decode is reused for the same bytes (a share's other targets).
- **One delivery per row across processes.** The app and the extension open the same database,
  so `OutboxLock` also takes a lease in `outbox_claim` (see [database.md](database.md)). The
  app's drain passes by a row the extension is delivering, and takes it over once the lease runs
  out. A delivery reads its row and its stored files in one read, so it never pairs a
  provisional envelope with files another process already uploaded. A chunk is written only into
  the row it reserved (checked by rowid, id and position).
- **`session`** — the iOS extension's own headless session, built like the Notification Service
  Extension's fetch (#1599): App Group data dir, shared keychain group, no realtime, LiveKit or
  media server, `LocalDb::open_existing_for_user_in`. It never reads the notification escrow:
  - **PIN** through `pin::check_pin`, the app's own attempt counter and lockout: a wrong PIN in
    the extension counts against the app's ten, a right one forgives them all, the tenth wipes
    the keys exactly as in the app. The attempt is counted before it is checked, under
    `pin.lock` (see [pin-design.md](pin-design.md)), so the two keypads cannot share a count.
  - **Biometrics** through `device_unlock::unwrap_with_device_key`, the app's device unlock key
    (#1685), which gates on the PIN counter and never writes it.
  - Every MLS step takes its share of the cross-process `state.lock` per section, as the app's do.
  - `suspend` / `resume` / `close`: when the host app leaves the foreground the sends in flight
    get ¾ of the time, then no new hold starts, the open ones drain and the database closes
    (`0xdead10cc`); anything unfinished is marked `failed`, its claims are released, and the
    app's next outbox drain delivers it. `resume` ends the suspension under the session's mutex,
    so a `suspend` still letting sends finish leaves the database open; a reopen that finds it
    busy is retried by the next call. `close` drains the holds before it releases the lock. When
    iOS's background time runs out, the host suspends at once (`timeout 0`).
- **`ffi`** — the C ABI, `pollis-core/include/pollis_share.h`: `pollis_share_open`,
  `_unlock_pin`, `_unlock_device_key`, `_targets`, `_send` (with a progress callback),
  `_suspend`, `_resume`, `_close`. Logs outcomes only (`[share] send: ok sent=2 queued=0`).
- **Keystore writes** take an exclusive `flock` on `keystore.pks.lock` around the file store's
  read-modify-write, since the app and its extensions share the file through the App Group and
  the extension writes the PIN's attempt counter.

## The Vault's durable queue (`commands/messages/vault_queue.rs`)

The Vault behaves like a chat with yourself; only its encryption differs. So every Vault save
(the Vault composer on desktop and mobile, its media, and shares) goes through a queue with the
message outbox's guarantees:

- `vault_outbox` (local) holds the row and `outbox_attachment` its files, written before any
  network work. The id is the client's from the first attempt (`send_vault_message` takes
  `messageId`), so a double submit or a retry saves one entry; the DS stores an entry by id.
- Delivery uploads the files under the Vault's file key (`kv: 0`), swaps in the finished
  envelope, saves the entry, writes `vault_entry_cache` and removes the row. A transient failure
  is retried in place after the outbox's delays (#2055), then the row reads `failed`; a refusal
  no retry fixes is `rejected`.
- `flush_outbox` drains it too (unlock, foreground, reconnect), and the suspend path marks an
  unfinished save `failed` like a message.
- `get_vault_messages` returns queued saves with their `send_state`; the UIs show "Not saved"
  with Retry (`retry_vault_message`) and Discard (`discard_unsent_vault_message`), and keep the
  composer clear: the draft was stored, and handing it back would save it twice. The media view
  leaves a queued save out until its files are uploaded.

## Hosts

- **iOS** (`mobile/plugins/withShareExtension.js`, Swift in `mobile/plugins/share-extension/`):
  the extension unlocks, shows a native picker and sends in place; it never hands off to the app.
  Files are copied with `loadFileRepresentation` into the extension's own temporary directory
  (never the App Group), HEIC becomes JPEG and HEVC becomes H.264 as the app's picker does, and
  each video gets a poster frame. Build, signing and portal steps: `mobile/CLAUDE.md` → "Share
  extension (#2100)". Harness: `mobile/plugins/share-extension/harness/run.sh`.
- **Android** (`ShareIntakeActivity`, `ShareInbox`, `PollisShareModule` in
  `modules/pollis-native`): the intake copies each `content://` stream into
  `cacheDir/pollis-share/<id>/` while it holds the read grant (only another app's provider:
  never a `file://` URI or Pollis's own; the intake runs as Pollis, so ownership is the
  provider's UID, a `user@authority` URI is refused, and an opened descriptor that is one of
  Pollis's own files is refused too), brings the app forward with a plain launch intent and
  finishes; not MainActivity, for the reason `PushTapActivity` exists (#1614). The app takes the
  share after the PIN (`hooks/useShareIntake.ts`, the push tap's gate) and pushes `app/share.tsx`.
  The share stays pending until it is stored or cancelled, so a crash or reload offers it again.
  Copies are deleted once stored, on cancel, at sign-out and after a day.

## Tests

- Rust: `share::targets::tests`, `share::send::tests`, `share::ffi::tests`,
  `r2::stream::tests`; flows `src-tauri/tests/flows/share.rs` (targets, one share to every target
  once with its own file keys, a repeated share sent once, the extension's send finished by the
  app, suspend/resume, one PIN counter with the app, biometrics never touching it, signed out,
  the Vault as a target, a row the extension holds left to it until its lease runs out, a share
  that fails part-way sending nothing); `pin::tests::parallel_wrong_pins_each_count`,
  `outbox::tests` (claimed bytes not swept, a chunk never written into another row) and `vault.rs` (a double-submitted save saved once, an interrupted save
  finished once, a file under the Vault key, a discarded save).
- Node: `share-picker`, `android-share-intake`, `share-extension`, `vault`.
- Desktop Playwright: `e2e/vault-queue.spec.ts`.
- Devices: `mobile/scripts/maestro-share.sh android` (adb `SEND` / `SEND_MULTIPLE`), the iOS
  harness, and the simulator walkthrough (Photos and Safari → Pollis).
