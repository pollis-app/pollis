# Changelog

What changed in each released version of the Pollis desktop app, written for the
person installing it.

**Why this file exists.** Every other link in the chain from "source" to "the
binary on your disk" is attested: the build has SLSA provenance and a Sigstore
signature, its hash is in a public append-only transparency log, and the Linux
payload is independently rebuilt from source after each release. Release notes
were the one unattested link — GitHub's auto-generated list of pull-request
titles, committed to by nothing and written for nobody. This file is the
human-authored answer to "what am I installing, and did anything change that I
should care about", and because it lives in the repository it is covered by the
same history and review as the code.

**How to check what you downloaded.** Every release publishes per-artifact
hashes, a `cosign` signature, a SLSA attestation and a CycloneDX SBOM. The
commands are on <https://pollis.com/artifacts>. Rebuild verdicts for each
release are at <https://pollis.com/assurance>.

Versions follow the `vMAJOR.MINOR.PATCH` tags in this repository. Dates are the
tag date, UTC.

---

## v1.19.12 — 2026-11-14

- **Release notes on the Software Update page.** The page shows what changed
  in the newest release: the update on offer when there is one, otherwise the
  version you are running.
- On Linux, the chat scrollbar no longer shows on top of an opened image.

## v1.14.0 – v1.19.11

These releases shipped between 2026-09-19 and 2026-11-14. What you would notice
in the desktop app:

**Calls**

- An incoming DM call takes over the window until you answer or decline, the
  caller hears a ringback tone while it rings, and the DM header offers Return
  to call while you are in one.
- Screen-share and camera resolution can be changed from inside a call.
- Voice has less delay, joining is faster, and a call that drops its connection
  reconnects in place.

**Messages and media**

- **An outgoing message is never silently lost.** Messages are stored before
  they are sent and delivered in the order you wrote them, and one that could
  not be sent says why on its own row. Edits are queued the same way.
- A message this device could not read shows a placeholder in its place.
- Sensitive media: mark an image or video as sensitive when you send it, and it
  stays covered until the reader reveals it.
- See who reacted: hover a reaction, or open View reactions.
- The media viewer steps through a message's images and videos with the arrow
  keys, and videos show a preview frame before they load.
- An image the app cannot decode is shown as a file.
- DMs are listed by latest activity, and a DM shows one receipt tick, on your
  newest message.
- A message you send from another device appears live, a DM or group created
  on one device shows up on your others, and unread state follows you between
  them.
- Channels can be muted. A muted channel notifies you only when someone
  mentions you.
- **Copy your message history** from one of your own devices to another.
- Group members: people you invited are listed under Invited, where an invite
  can be revoked, and new members are tagged. Accepting an invite shows the
  group at once.
- Any member can add custom emoji; only admins can remove them.
- Report a user or a message. A report carries identifiers and your reason,
  never message content.
- On Linux, pasting from a slow clipboard no longer stalls the window.

**Devices and security**

- **Sign in a new device with a QR code**: the new device shows the code and a
  device that is already signed in scans it, with no email code.
- The safety number is in the DM itself ("You & name"), and a change of safety
  number is noted in the conversation.
- A device you revoke is told so and signs itself out.
- Security events can be paged and filtered.
- Attachments are sealed under a per-conversation key ring carried by the
  conversation's own encryption.
- The app can require an update when a release is too old to work with the
  service.

**Updates and diagnostics**

- Linux `.deb` and `.rpm` installs update themselves from inside the app.
- A failed update offers Try again, the download page, and Continue without
  updating.
- A Diagnostics page in Settings keeps an on-device log you can copy or save as
  a file.
- The open-source licences are listed in the app.

**Look and feel**

- Buttons that only navigate act on press rather than on release.
- Terminal view: attachments and reactions line up with the message text, and
  other admins' name chips are filled with their own colour.

Reliability and security improvements throughout.

## v1.13.0 — 2026-09-18

- **Notifications do not tell Apple, Google or Expo which conversation they
  are for.** A push carries a single random handle, different every time,
  which your device trades for the real destination over its own authenticated
  connection.
- Deleting a message requires proof that you are allowed to, on every message.
- Account keys are wiped from memory on the paths that create them.
- The retired sidebar-index tables were removed from the database.

## v1.12.0 — 2026-09-14

- Upgraded the TLS library.
- Message delivery orders on a server-assigned sequence rather than a
  timestamp from the sending device, so a wrong clock on someone else's machine
  cannot delay, bury, or lose a message — it can only make a timestamp look
  odd.
- **Approving a new device asks you to type the code it shows**, so the
  comparison the code exists for is actually performed.
- Sign-in codes: the guess limit and lockout are now per mailbox rather than per
  request, and email addresses are matched case-insensitively.
- Push tokens are bound to the device that registered them, so a token cannot be
  pointed at someone else's account.
- Group membership bookkeeping: a departing member is removed promptly, and
  multi-user additions record every added user.

## v1.9.8 – v1.11.4

These releases shipped between 2026-08-16 and 2026-09-04. Their changes are
recorded in the git history and on the GitHub release pages.

## v1.9.7 — 2026-08-16

- Dates, keyboard glyphs and role labels now localize correctly, and arrow-key
  navigation follows reading direction in right-to-left languages.
- The right-hand context panel remembers whether you left it open, per account
  and per device, instead of storing that in the URL.
- Message composer fixes: emoji centering, skin-tone swatches, and the message
  actions overflow menu. Added arrow-key navigation of the message log.
- DM member lists show the actual participants rather than an empty roster.
- Backend cleanup pass: removed drifted duplication, tightened several hot
  paths, and made the test fixtures use the real schema.

## v1.9.6 — 2026-08-15

- **The app is now localized into seven languages**, including right-to-left
  layout.
- **Auto-lock**: Pollis locks itself after a period of inactivity and requires
  your PIN to unlock.

## v1.9.5 — 2026-08-15

- **@mentions** that actually notify the person mentioned.
- **Saved items and permalinks**, built so a shared link discloses nothing about
  the conversation to anyone who cannot already read it.
- **Shareable group invite links**, with hashed and rate-limited tokens.
- **A full emoji picker**, plus custom per-group emoji with global
  de-duplication.
- **Delivery and read receipts for DMs.**
- **Voice**: push-to-talk, self-deafen, and input-mode selection.
- Several voice and message-copy fixes.

## v1.9.4 — 2026-08-14

- **Message deletion is scoped to the conversation you are authorised in.**

## v1.9.3 — 2026-08-13

- Fixed video in the call stage being clipped at small window sizes.

## v1.9.2 — 2026-08-13

- Fixed release provenance by pinning the `cosign` installer to an exact
  version — the moving major tag it had been using does not exist.

## v1.9.1 — 2026-08-13

- **Thread isolation fixes**, plus skin-aware panels and contextual details.
- **Pseudonymous LiveKit room names**, so the media server cannot map a room
  to a conversation.
- Fixed a case where the client could pick an external join with no group state
  to join onto.
- Cut input lag in the terminal client on Linux.

## v1.9.0 — 2026-08-12

- **Slack-style threads in channels.**
- **A right-hand context panel** showing members and shared media.
- **Relay overlay v1**: multi-hop onion routing, peer-hosted relays, live
  revocation, and anchoring in the transparency log. Opt-in and off by default —
  see <https://pollis.com/subprocessors> for how the pool is operated.
- Calls: TURN over 443 for restrictive networks, and a wider relay port range.
- Arch packaging: the desktop entry pins an absolute path to the Pollis
  binary.

---

## Earlier releases

Versions before v1.9.0 predate this file. Their contents are in the
[GitHub releases](https://github.com/pollis-app/pollis/releases), which carry
the auto-generated pull-request list, and every one of them has its hashes in
the public transparency log regardless of how its notes were written.

## How this file is written

Entries are written when a release is cut, from the pull requests merged since
the previous tag, and describe each change from the user's side: "Message
deletion is scoped to the conversation you are authorised in" is useful;
"fix(delivery): scope message deletes" is not.

An entry's heading is `## vX.Y.Z — YYYY-MM-DD`, exactly, and the entry is
written before the `vX.Y.Z` tag is pushed. The release reads the text under
that heading (`scripts/changelog-section.mjs`) and publishes it with the build:
it is what the desktop app shows on its Software Update page, and the top of the
GitHub release. A release without an entry still ships, with no notes. Keep an
entry to paragraphs, `-` bullets, `**bold**` and `` `code` ``; the app shows
anything else as plain text. A heading that spans versions
(`## v1.14.0 – v1.19.11`) belongs to no single release and is never shown in
the app.
