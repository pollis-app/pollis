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

## v1.19.17 — 2026-11-18

- **A different name and picture in each group.** You can now show a separate
  display name and profile picture in any group. Your @handle stays the same
  and is shown beside the name, and everywhere outside that group (direct
  messages included) you keep your main profile. Profile → Group aliases lists
  every group: change one, apply a name or picture to all of them at once, or
  go back to your main profile. Group admins can reset a member's alias.

## v1.19.16 — 2026-11-17

- **Real device names on Linux.** The microphone and speaker pickers list your
  devices by name ("AirPods Pro", a USB microphone) instead of "PulseAudio",
  "PipeWire" or "JACK", Bluetooth headsets included. A call on the default
  device follows a headset you connect mid-call, and a device you unplug falls
  back to the default.
- **Approving a join request is quick.** Approve answers in about a second;
  adding the person to the group's encryption finishes in the background.
- **Faster unlock after you join a group.** The app no longer waits on the
  group's follow-up key rotation before showing your groups.
- **Sign-in looks like your app.** The sign-in, code and PIN screens use your
  last look (Modern or Terminal, and your colours) instead of the defaults.
- **A tidier Modern look.** One focus ring on every control; buttons, fields
  and corners match across the app; page titles are headings; every page has a
  full breadcrumb; terminal-style brackets, spinners and spaced capitals stay in
  Terminal; selections use a tint instead of a solid colour.
- Confirmations look the same everywhere: the question, what it will do, then
  the action and Cancel. Cancel has the focus, so Enter never deletes by
  accident.
- Empty lists say so in one centred sentence, with the action that fills them.
- An invite link names the group before you join it, and a few labels and
  counts read correctly (approving a device, the open-source list, group
  search, emoji order).

## v1.19.15 — 2026-11-17

- **Adding a Windows computer to your account works.** Signing in on a new
  Windows device, by approval from another device or with your Secret Key, now
  finishes instead of failing at the last step.
- Privacy hardening for the Windows app.
- When the server turns a request down, you now see what to do about it (for
  example, check your device's date and time) instead of a raw error line.
- Korean text wraps between words rather than in the middle of one.
- Approving a request to join a group no longer waits on unrelated work, and
  shows that it is busy while it runs.

## v1.19.14 — 2026-11-16

- **Seventeen more languages.** Pollis now also comes in Korean, Japanese,
  Urdu, Hindi, Punjabi (Shahmukhi and Gurmukhi), Pashto, Bengali, Marathi,
  Telugu, Tamil, Swahili, Portuguese, Hausa, Amharic, Yoruba and Zulu, 24 in
  all. Right-to-left languages lay out right to left.
- **Language has its own page.** Preferences shows your current language on
  one row; the full list opens on its own page instead of filling Preferences.
- **Modern is now the default look** for a new install or an account that has
  never picked one. Terminal is still a choice in Preferences.
- Every destructive action (delete, leave, remove) uses the same red button.
- Messages that can't be shown now say so in plain words: "Couldn't decrypt",
  "Sent before you joined" for a quote from before you joined, and "Message
  deleted", instead of bracketed tags.
- The side panel follows what you are looking at: a group's online members,
  the people in a DM, or otherwise your online contacts.
- The sign-in code step now asks for the code rather than repeating your
  email.

## v1.19.13 — 2026-11-15

- **Choose your look on first sign-in.** A new account picks Terminal (the
  default) or Modern before the app opens, so the second theme no longer has
  to be found in Preferences. The theme formerly called Refined is now called
  Modern.
- Opening Pollis while it is already running, for example after closing the
  window to the tray, brings the open window forward instead of starting a
  second copy. Separate installs, OS users or data directories still run side
  by side.
- A custom emoji from a group you are no longer in is sent as its plain
  `:name:` text, whether you send or edit the message.

## v1.19.12 — 2026-11-14

- **Release notes on the Software Update page.** The page shows what changed
  in the newest release: the update on offer when there is one, otherwise the
  version you are running.
- On Linux, the chat scrollbar no longer shows on top of an opened image.
- People no longer show as offline while they are connected: someone using
  Pollis on two devices stays online when one of them leaves a conversation,
  and so does the other person after a DM call ends.

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
