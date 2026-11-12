# Pollis mobile — Maestro e2e + visual suite (#619 / #621)

Automated flows that drive the app on **iOS Simulator, iPad Simulator, and
Android emulator**, capturing a **screenshot gallery** at each meaningful state
so a human/visual evaluator can confirm quality or spot defects across all three
form factors.

> **Runs on a local Mac only.** This suite was *authored* in a headless Linux
> box that cannot run Maestro, simulators, or emulators — so the flows are
> written best-effort against the real `testID`s (see `SELECTORS.md`) and need a
> first-run shakedown on the Mac (timing waits, and the small `testID` gaps noted
> below). Decision log: Maestro / local-Mac / full adaptive iPad, in
> `mobile/TEST-PLAN.md`.

## Prerequisites (Mac)

1. **Maestro** — `curl -Ls "https://get.maestro.mobile.dev" | bash`.
1b. **A Java runtime.** Maestro is a JVM tool and dies with "Unable to locate a
   Java Runtime" without one. `brew install openjdk@17` is keg-only, so
   `/usr/libexec/java_home` cannot see it and installing alone is not enough —
   export it:
   ```bash
   export JAVA_HOME=/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home
   ```
2. **A RELEASE build of the app, pointed at the dev DS.** Both halves matter.

   *Release, not dev-client.* Every flow opens with `clearState`, which wipes
   expo-dev-client's stored dev-server URL — so a Debug (dev-client) build
   boots to the **dev-client launcher menu** and the app never loads. The suite
   cannot drive that build at all. A Release build embeds the JS bundle and has
   no launcher. It is also the more honest target: these flows are
   timing-sensitive and dev-mode JS is not what ships.
   Xcode 27 ships no Simulator.app, so `expo run:ios` fails (`Can't determine
   id of Simulator app`); build and install with xcodebuild/simctl instead
   (full sequence in `mobile/CLAUDE.md` → "Dev loop — iOS"):
   ```bash
   cd mobile
   xcrun simctl boot <simulator-udid>
   xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
     -configuration Release -destination id=<simulator-udid> \
     -derivedDataPath ios/build build
   xcrun simctl install <simulator-udid> \
     ios/build/Build/Products/Release-iphonesimulator/Pollis.app
   ```
   Address the simulator by **UDID** (`xcrun simctl list devices available`)
   and boot it first; a name that matches no *booted* simulator makes xcodebuild
   fall through to a physical-device destination and fail.

   *Dev DS.* The build must point at the dev Delivery Service (baked at build
   time, not Maestro env):
   ```
   EXPO_PUBLIC_POLLIS_DELIVERY_URL=https://api-dev.pollis.com
   EXPO_PUBLIC_LIVEKIT_URL=wss://<dev-livekit>
   # No database credential is needed or read since #987 — the client speaks
   # only to the Delivery Service.
   ```
   See `mobile/CLAUDE.md` for the ubrn/native steps. App id: `com.pollis.ios` on iOS, `com.pollis.mobile` on Android — flows read `${MAESTRO_APP_ID}`, which `scripts/maestro-app-id.sh` sets per device. Running `maestro` by hand, export it first.

   *After changing the ubrn target set* (e.g. building simulator-only slices),
   re-run `pod install`: CocoaPods bakes the xcframework's slice paths into a
   generated copy script, so a stale script silently copies **nothing** and the
   app dies at launch on a uniffi checksum mismatch.
3. **Seed env** — `cp .maestro/env.example .maestro/.env` and fill it in. The dev
   DS must have a fixed `DEV_OTP` so `MAESTRO_OTP` is deterministic (no inbox
   polling). `.env` is gitignored.

## Run

```bash
# one flow on the iPhone simulator
mobile/scripts/maestro-run.sh auth ios
# the whole suite on the iPad simulator (exercises the #622 two-pane)
mobile/scripts/maestro-run.sh all ipad
# on Android
mobile/scripts/maestro-run.sh messaging android
```
Screenshots land in `mobile/.maestro/artifacts/<date>/<platform>/` — that's the
gallery the visual evaluator reviews. Override device names with `IOS_DEVICE=…`,
`IPAD_DEVICE=…`, `ANDROID_AVD=…`. The defaults are Xcode 27's `iPhone 18 Pro`
and `iPad Pro 13-inch (M5)`, and the `pollis_e2e` AVD (Pixel 8, API 36
`google_apis` arm64 — see `mobile/CLAUDE.md` for creating it). Simulator names change with each Xcode, so the
script resolves the name among *available* simulators and, if none matches,
exits printing the ones that exist rather than running with no `--device` at
all:
```bash
IOS_DEVICE="iPhone 17 Pro" mobile/scripts/maestro-run.sh auth ios   # an Xcode 26 machine
```

## Flow matrix (`flows/`)

| Flow | What it proves | Peer? |
| --- | --- | --- |
| `auth` | email→OTP→PIN→inbox (also the smoke test: bridge + dev DS + keystore live), then a cold relaunch: a wrong PIN puts the pad back with `pin-error`, and the right one's fourth digit must, within a second, show `pin-busy` (the launch loader, with no keypad key left on screen) or have already left the pad (a fast unlock; never a frozen pad) | no |
| `groups` | create group, open, channel visible | no |
| `messaging` | send / edit / delete / react in a self-owned channel | no |
| `composer-draft` | unsent composer text survives backing out to another tab and returning (desktop's in-memory draft store, `mobile/lib/drafts.ts`); sending clears it | no |
| `outbox` | #1335 a message whose send is killed mid-flight (`stopApp` right after Send in a fresh group) is still there after relaunch + unlock and is delivered by the outbox drain (no `btn-retry-message-*` left); Android only: a send made in airplane mode is kept as failed with `btn-retry-message-<id>` / `btn-discard-message-<id>`, and back online Retry settles it | no |
| `long-history` | #1432 seeds a channel with more messages than one screen holds (each send made with the keyboard up, so a composer left behind the keyboard fails the tap), then re-opens it: `input-composer` and `btn-send` must be on screen | no |
| `keyboard-recovery` | #1641/#1642 the layout comes back after every keyboard hide: navigating away with it up, backgrounding with it up (Android hides it with no animation), and the scan-to-sign-in PIN step that unmounts the focused field. After each, the `keyboard-closed` probe and a point tap at 50%,92% that must land on the bottom `btn-scan-toggle-manual` (tune the point from shot 01 if a device's inset moves it). The PIN step, which opts out of keyboard avoidance (`<Screen avoidKeyboard={false}>`), must take a point tap at 50%,90% on its bottom-row `0` key. Then (#1662, #1687) a focused chat composer across Home, the link sheet's Open in browser, the browser opened with the keyboard up, and Home then the browser: each time Send is tapped by id and the message must land, so a composer left under the keyboard fails. Proves the native keyboard-controller patch, so it needs a fresh binary | no |
| `message-links` | #1682 a long press on a URL in a message opens the link sheet (not the message sheet) with its host, Copy link copies (checked by pasting the system clipboard back into the composer, not only by the 2 s "Copied" label, #1813), Open in browser leaves the app for the system browser; a tap on an `xn--` host opens the confirm sheet showing the ASCII host; a `javascript:` URL stays plain text. Each message is one long wrapping link, because a nested `<Text>` span has no id and a text selector taps the row's centre | no |
| `reply` | #1270 swipe a message toward the start edge → reply bar (not the message sheet), send, the row quotes the original; double tap does the same; dismiss cancels; tapping a quote jumps to the original | no |
| `channel-menu` | #1193 the channel kebab sheet: every item tapped by `testID`, plus `channel-menu-01-open` — the gallery shot that must show an OPAQUE sheet (no header/composer through its buttons; the invariant itself is pinned in `tests/sheet-opaque.test.ts`) | no |
| `profile-prefs` | accent re-theme, behavior toggle, display-name save | no |
| `theme-startup` | #1447 pick lilac, then a cold restart: the first screen (PIN unlock or tabs) carries `theme-boot-accent-bda3e0` and no amber probe before any tap, and the tabs stay lilac after unlock; a fresh account settles on amber even with lilac left in the device store | no |
| `search` | #1202 desktop parity: a seeded message hit with highlighted snippet + "About N results", sort toggle, corpus footer, opening a hit, settings-page quick-jump, and the "why no results" explanation | no |
| `security` | device list + blocked-list entry | no |
| `security-events` | #1548 Security shows the newest three events + "View all"; the Security events screen loads its next keyset page on scroll and the All / Devices / Identity & keys / Account chips narrow it (`recovery_blob_fetched` has its own copy); #1547 no Recovery card, `?section=data` lands on "Your data". The paging half needs a log longer than a page: against a local DS set `MAESTRO_SQLD_URL` (its sqld) and `scripts/seed-security-events.js` writes 40 rows; without it those steps are skipped | no |
| `client-policy` | #1611 a build below the published minimum: the dev override link blocks a running app (`screen-client-blocked`, Android back stays put), a cold launch with the override still set is blocked before any sign-in screen, Check again re-checks with a busy state, and clearing the override (the unpublished dev file 404s, i.e. no minimums) unblocks back to sign-in and stays so across a cold launch. Overrides are never cached; the offline cached-block case is in the core and CLI tests. Needs no account | no |
| `app-update` | #1443 Self → App shows the native version + build number and the Update row with its destination (a Maestro build is the `dev` channel, so it names pollis.com) | no |
| `media-viewer` | #1248 send an image through the SYSTEM photo picker (seeded by `addMedia` from `fixtures/`, cell picked by its accessibility label), cold-restart, and require the image to render from R2 (`media-image-rendered`, never `media-unavailable`); then the full-screen viewer: double-tap zoom, Save to photos with feedback, Share reachable, Close. Every step is mandatory | no |
| `media-recovery` | #1826 a failed image is never stuck on "Image unavailable": send an image (as `media-viewer`), cold-restart, then on Android open the chat in airplane mode (`media-unavailable`), turn the network back on, check the row does NOT recover on its own (nothing polls), open the viewer (which fetches the file) and close it: the same, never-remounted row is `media-image-rendered`. iOS has no Maestro network switch, so it stops after the online render (the recovery is unit-tested in `tests/media-recovery.test.ts`) | no |
| `vault-media-instant` | Vault Chat ↔ Media: an image entry, then three switches each way, each of which must already show `media-image-rendered` (never `media-image-loading`) on the first look. The switch is faster than Maestro can see on a simulator, so the frame-by-frame proof is a screen recording (the PR's before/after); the flow guards a regression to a slow re-decrypt | no |
| `vault` | #1316 Direct → Vault: empty state, save two entries, pin, edit (pin survives), search filters, media view, a best-effort image entry opening the viewer from the grid, delete asks first (cancel keeps it, confirm removes it), re-open re-syncs from the DS | no |
| `export` | #856 on-device archive from Security: summary renders, share button reachable, no network offer on an attachment-free account | no |
| `ipad-two-pane` | #622 list+detail side-by-side (run on **iPad**) | no |
| `dms` | start a DM with the seeded peer (initiator side) | yes |
| `custom-emoji` | #1562 add a static and an animated custom emoji, see both as images (never the `:shortcode:` text) in the group's list, the composer's suggestions, a sent message and again after a cold restart | no |
| `deep-link` | #1223 a `pollis://` link routes both warm (scene `openURLContexts`) and cold (scene `connectionOptions` → `Linking.getInitialURL`) under the UIScene life cycle | no |
| `i18n` | switch language (copy re-renders), persists across relaunch, Arabic mirrors the layout after relaunch (#1074) | no |
| `enrollment-approval` | #1096 the approver TYPES the code off the new device — the card shows none, approve is gated on eight characters | yes |
| `scripted/biometric/` (`scripts/maestro-biometric.sh <ios\|ipad\|android>`) | #1685 biometric unlock: off by default, offered once after a PIN unlock, Security shows it on, a cold start tries biometrics first, a failed match falls back to the keypad with no PIN error, linking still checks the PIN, a PIN change keeps it working, the switch asks for the PIN to turn on, off is PIN-only again. Maestro cannot touch a sensor, so the script enrolls the simulator's Face ID (`notifyutil … BiometricKit`) or the emulator's fingerprint (`locksettings set-pin` + `emu finger touch`) and answers each prompt between six parts; it unenrolls on exit so no other flow meets the offer | no |

Two-client / special flows (`enrollment-approval`, `realtime`, `blocking`,
`push-tap`, and the DM accept/reply side) are scaffolded in `_two-client.md` —
they need the two-device setup below and a Mac shakedown. `maestro-run.sh all`
skips every flow tagged `two-client` (on one device they can only fail) and
names them at the end: `report`, `dm-safety` and `dms` run in
`scripts/maestro-report.sh`, device approval in `scripts/maestro-approval-prompt.sh`.

## Screen tour (visual audit)

`flows/tour.yaml` (tag `audit`) signs up, makes a group with a message, and
takes one numbered screenshot of every reachable screen (`tour-01-…` to
`tour-33-…`), so a layout or copy change can be compared before and after. It
asserts nothing; collect the shots with `--debug-output <dir>`.

## Visual before/after comparison

For a re-skin or any layout change, shoot the tour on the old build and the new
one, then pair them up:

```bash
# 1. "before": the tour on the pre-change Release build (kept, gitignored, at
#    artifacts/baseline-before/{ios,android}/ — 34 shots each, 2026-10-04/05)
# 2. build + install the working tree, then shoot "after"
mobile/scripts/build-release-sims.sh both            # or ios | android
mobile/scripts/maestro-run.sh tour ios               # -> artifacts/<date>/ios/tour-*.png
mobile/scripts/maestro-run.sh tour android
# 3. compare
NAME=ios mobile/scripts/visual-compare.sh \
  mobile/.maestro/artifacts/baseline-before/ios mobile/.maestro/artifacts/<date>/ios
open mobile/.maestro/artifacts/compare/<run>/index.html
```

`visual-compare.sh` pairs `tour-NN-*.png` by name (falling back to the `NN`
number when a slug was renamed) and writes, under the gitignored
`artifacts/compare/<timestamp>[-NAME]/`: `side/<name>.png` — one labelled
BEFORE | AFTER image per pair (composited by a small Swift helper, so nothing
to install; one image per screen is also what a reviewing agent can open);
`index.html` — the contact sheet, every pair side by side with a
changed / identical / only-before / only-after tag and a "hide identical"
toggle; `summary.txt`. The tag is a byte comparison, not a perceptual diff:
the status-bar clock and each run's fresh signup handle make practically every
shot "changed" even between two runs of the same build, so the review is by
eye. Set `PREFIX=` to compare a gallery other than the tour.
Screenshots are never committed — `artifacts/` is gitignored.

## iOS driver under Xcode 27

Maestro 2.11.0's iOS driver never comes up under Xcode 27 ("iOS driver not
ready in time", on every simulator, whatever `MAESTRO_DRIVER_STARTUP_TIMEOUT`
is): its bundle runs a unit-test class before the HTTP-server test, and Xcode
27's XCTest then blocks after the first test in
`XCTCrashLogTracker.waitForPendingCrashlogs()`, so the server never starts.
`mobile/scripts/maestro-ios-driver-fix.sh` skips that class in the driver's
`.xctestrun` inside `~/.maestro/lib/maestro-ios-driver.jar` (backup kept as
`.orig`, `--revert` restores it). `maestro-run.sh` applies it on every iOS run,
so a Maestro reinstall heals itself; drop both once Maestro ships the fix.

## Seeded photos (`addMedia`, #1769)

`addMedia` adds its fixture to the device library and never removes it, and
on Android the system photo picker does not see what it adds.
`mobile/scripts/maestro-media.sh` handles both, and nothing calls it by hand:

- **Android:** the `maestro` wrapper in `scripts/maestro-app-id.sh`, which every
  script sources, runs `maestro-media.sh reset` before each flow file holding
  `- addMedia:`, then `maestro-media.sh watch` until the flow ends. `reset`
  deletes the fixtures, their numbered copies (`media-viewer (7).png`) and the
  app's "Save to photos" files in `DCIM/` through MediaStore. Android stops
  numbering at 32 copies of one name, and every later `addMedia` of it then
  failed with a bare gRPC `UNKNOWN` (the 1.0.32 gate). It then checks the
  picker's own database against MediaStore (root adbd, which the userdebug
  emulator has) and forces a full picker resync if they differ, so no reboot
  is needed. `watch` asks MediaProvider to sync the picker each time MediaStore
  changes: the row `addMedia` writes is completed without the notice that
  would do it, so the picker never listed a freshly seeded photo, and the
  flows used to pass only by picking an older copy left by an earlier run.
- **iOS:** the library cannot be edited from outside, so `maestro-run.sh` (and
  the custom-emoji and group-emoji scripts, for an iOS seeder) reset it once
  per run: shut the simulator down, delete `data/Media/DCIM` and
  `data/Media/PhotoData`, boot. Photos makes an empty library.

## Two-client flows

**Join requests (automated):** `mobile/scripts/maestro-join-request.sh <admin-device> <peer-device>`
— the admin creates a group and opens it from the Groups tab's group pill
strip; the peer finds it by slug (Groups "+" → Find group, one lookup) and
requests access; the admin sees the request on the Groups tab (pending row +
badge, above the channels) and approves it; the peer then lists the group.
Step 5 (`5-peer-settings.yaml`, #1281) opens Group settings as that plain
member: the name is text (`value-group-name`), and with no description there
is no Description field at all — no inputs, no Save. Step 1 checks the
admin's own settings are still the editable form.

**Group emoji permissions (automated, #1280):** `mobile/scripts/maestro-group-emoji.sh <admin-device> <peer-device>`
— the join-request steps make the peer a plain member of the admin's group; the
peer then adds a custom emoji (system photo picker, seeded by `addMedia`) and
must see no remove control for it, not even as its creator; the admin then sees
the remove control and removes it.

**Custom emoji render (automated, #1562):** `mobile/scripts/maestro-custom-emoji.sh <sender-device> <recipient-device>`
— the sender creates a group with #General and adds a static (PNG → WebP) and
an animated (GIF) custom emoji, the recipient joins through the join-request
steps, the sender posts both, and the recipient must see both as images
(`emoji-image-<shortcode>`) with no `emoji-fallback-*`. Run it iOS → Android
and Android → iOS. Those ids come from `EmojiProbe`, drawn by `<Screen>`: an
emoji sits inside a row that is one accessible element, so its own view is
not in the hierarchy Maestro reads.

**Report (automated, #1213):** `mobile/scripts/maestro-report.sh <reporter-device> <peer-device>`
— a fresh peer signs up, its handle is read off the screen, and the reporter
reports it with "Report and block", then unblocks. Then, each as another fresh
account, `dm-safety` (the safety number in a DM with the peer) and `dms`
(starting a DM with it).

**Composing (automated, #1507):** `mobile/scripts/maestro-composing.sh <device>`
— one device plus `pollis-tui`'s `composing_peer` example. The device types a
draft with the keyboard up; the peer then sends into the open channel, twice.
The composer must still be above the keyboard: the draft is sent by tapping
Send where it is. Each arrival is screenshotted straight off the device.

**Live message (automated, #1610):** `mobile/scripts/maestro-live-message.sh <device>`
— one device plus `pollis-tui`'s `live_message_peer` example, which sends when a
flow asks it over loopback HTTP (`scripts/peer-send.js`), so a step sends and
then waits with a tight timeout: a message into the open DM is on screen within
2 s of the DS having it; one stored while on the Direct list shows the moment
the DM opens, with no loading line; and after 35 s in the background one is in
the DM within 3 s of opening it on resume.

**Typing indicator (automated, #1635):** `mobile/scripts/maestro-typing.sh <device>`
— one device plus `pollis-tui`'s `typing_peer` example (built with
`--features typing-peer`, i.e. pollis-core's real LiveKit path, so it types
exactly as desktop does). `two-client/typing/`: the peer types and
`text-typing-indicator` reads "<peer> is typing…", then goes when it stops; the
device types and the peer sees `typing`, then `stopped` when the draft is
cleared and again after a send (`scripts/peer-typing.js`).
**DM receipts (automated, #1637):** `mobile/scripts/maestro-receipts.sh <device>`
— one device plus the same `live_message_peer`, which also fetches the DM and
marks the device's messages read when a flow asks (`scripts/peer-read.js`). Two
messages read by the peer show ONE tick, on the newest (the row's label ends
", Read by everyone"); the peer's reply carries none and does not move it; a
third message read by the peer takes the tick from the second.

**Device outside the tree (automated, #1571):** `mobile/scripts/maestro-device-outside-tree.sh <device>`
— one device plus two dev-DS peers (`pollis-tui`'s `outside_tree_peer`, roles
`owner` and `creator`). The owner signs up as the account's first device; the
device enrolls into it with the recovery key and closes the app
(`two-client/device-outside-tree/1-enroll.yaml`). With the app closed, the
creator spends the device's regular KeyPackage pool (five group invites to the
account), opens a DM with the account, and exchanges three messages with the
owner device. The device then opens the app for the first time since and must
show all three in the DM with no placeholder (`2-open-dm.yaml`) — its
last-resort KeyPackage is what added it. The DS must carry #1571.

**Lost message (automated, #1312):** `mobile/scripts/maestro-lost-message.sh <device>`
— one device plus a dev-DS peer client, not a second device: the script runs
`pollis-tui`'s `lost_message_peer` example (a `test-harness` build that never
ships), which signs up a fresh peer and prints its handle. The device signs up,
opens a DM with it (`two-client/lost-message/1-open-dm.yaml`); the peer accepts
and sends four messages with the middle two corrupted; then the device
re-opens the DM and must show the readable two with ONE collapsed "2 messages
couldn't be decrypted" placeholder between them, whose link opens Diagnostics
(`2-see-placeholder.yaml`). The dev gate
key is read from Doppler `dev_personal` when `POLLIS_DEV_ACCESS_KEY` is unset.

**Device revoked while closed (automated, #1338):** `mobile/scripts/maestro-device-revoked.sh <device>`
— the `revoke_peer` example signs up a fresh account as its first device and
prints the recovery key; `two-client/device-revoked/1-enroll.yaml` joins that
account on the device with the key and closes the app; the peer revokes the
device; `2-revoked.yaml` reopens and unlocks and must land on sign-in with
`text-session-notice` ("removed from your account").

**A group reaches the user's other device, both ways (automated):** `mobile/scripts/maestro-own-groups.sh <device>`
— `pollis-tui`'s `own_groups_peer` example signs up a fresh account as its
first device (the stand-in for the user's desktop) and prints the recovery
key; `two-client/own-groups/1-enroll.yaml` joins that account on the device.
`2-phone-creates.yaml` creates a group with #General — which must show on the
phone with another device in the account — and posts in it; the peer must list
the group and read the post (the create admitted its device). The peer then
creates a group, and `3-peer-group-live.yaml`, which never touches the phone,
must see its pill appear: only `create_group`'s inbox wake-up can do that.

**Copy message history between own devices (automated, #1735):**
`mobile/scripts/maestro-history-transfer.sh <device>` — `enroll_peer`
`approver --seed-history N --transfer-source` signs up an account, posts N
messages (`history-seed-01`…) in "History Test Group" #General and approves the
phone's sign-in code. `two-client/history-transfer/1-receiver-copies.yaml`: the
phone sets its PIN, is offered the owner's history (`screen-history-offer`),
copies it — the peer approves with its PIN and sends — reaches
`history-transfer-done`, and must show the oldest and newest seeded message,
each exactly once, with no undecryptable row (`assert-no-loss`). Then the other
direction: `enroll_peer new --transfer-receiver` joins as a second device (the
phone approves its code), asks for the phone's history, and
`2-phone-approves-device.yaml` approves it, and
`3-source-approves.yaml` must see the approval page open on its own, take the
PIN and show the send to its end; the peer reports every seeded row received.
`4-security-shows-events.yaml` finds "Message history shared" in Security and
opens the device picker. `5-qr-link-inline.yaml`: a signed-out peer (`enroll_peer
offer - --transfer-receiver --transfer-via link`) shows its code, the phone
scans it (typed in) and confirms with its PIN, and the new device's request must
appear INLINE on the "Linked" page as one tap with no keypad, then send to its
end. `6-receiver-kill-resume.yaml`: a fresh account seeded with `BIG` (3000)
messages padded to `BIG_PAD` (4000) bytes, so the copy spans ~25 chunks; the
source peer runs through `scripts/slow-proxy.py`, switched on only once the copy
starts, so the kill cannot miss. Once at least one chunk has landed the app is
killed (`killApp`) and relaunched; after the PIN the copy must resume by itself
to exactly "Copied 3000 messages." with the newest message shown once and
nothing undecryptable. `ONLY_KILL=1` runs that step alone, `SKIP_KILL=1` skips
it, `KEEP_WORK=1` keeps the peers' logs. Every other new-device flow says "Not now" to the
offer (`subflows/skip-history-offer.yaml`). Until the dev DS carries the #1735 routes it
answers 404 to them; run it against a local DS instead (a `sqld` stamped with
`pollis-schema`'s scripts, `pollis-delivery` with Doppler `dev_personal` plus
`TURSO_URL` pointing at it), a build whose `EXPO_PUBLIC_POLLIS_DELIVERY_URL` is
`http://localhost:<port>` (Android: `adb reverse tcp:<port> tcp:<port>`), and
`POLLIS_DELIVERY_URL=http://localhost:<port>` for the peers.

**A new device's approval request comes to the phone (automated, #1360):**
`mobile/scripts/maestro-approval-prompt.sh <device>` — `pollis-core`'s
`enroll_peer` example plays the desktop on the desktop's own core commands
(built `--no-default-features`, so it never touches the macOS Keychain). The
phone signs up and sits on the Groups tab; three peer "desktops" then each
press "Approve from another device" — while the app is open (the DS's live
nudge must open `screen-approve-device`), while it is closed (it must appear
after reopen + unlock) and while it is backgrounded (it must appear on resume;
this one is rejected and the peer must see the rejection). No step opens
Security. Then the other way: a peer signs up a second account, the phone
signs in to it as a new device, its code is read off the screen and handed to
the peer, which approves with `approve_device_enrollment`, and the phone must
land in the app. `maestro-device-link.sh`'s step 3 likewise waits for the
approval screen to open by itself.

**Who reacted (automated, #1684):** `mobile/scripts/maestro-reaction-breakdown.sh <device>`
— a dev-DS peer (`pollis-tui/examples/reactions_peer.rs`) and a fresh account
on the device open a DM; the peer posts a message and reacts 👍 and 🎉, the
device adds its own 👍 with a short tap on the pill (the toggle still works),
then long-presses the pill: the sheet opens on the 👍 tab (and the message
sheet does not), lists both accounts there — the device's own row marked
"(you)" — only the peer under 🎉, and both on All. The peer exits once it sees
the device's 👍. `SHOTS_DIR=<dir>` keeps the screenshots.

**Pins across members (automated, #1315):** `mobile/scripts/maestro-pins.sh <device>`
— the lost-message shape with `pollis-tui`'s `pins_peer` example: the device
opens a DM with a fresh peer (`two-client/pins/1-open-dm.yaml`), the peer sends
a message and pins it, then the device must list the peer's pin, offer "Unpin
message" on it, pin its own message and unpin the peer's
(`2-pin-across-members.yaml`); the script passes only when the peer has seen
the device's pin and unpin in its own list. The one-device half (pin, list,
jump, unpin) is `flows/pins.yaml`.

**Video posters across clients (automated, #1837):** `mobile/scripts/maestro-video-posters.sh <device>`
— the pins shape with `pollis-tui`'s `video_poster_peer` example (needs the dev
R2 origins, read from Doppler `dev_personal`): the device opens a DM with a fresh
peer (`two-client/pins/1-open-dm.yaml`); the peer uploads two real videos, one
with a poster frame and one without (as a build from before #1837 sent them);
the device must draw `video-poster-pv` for the first and `video-poster-local`
(a frame decoded on the phone) for the second, never `video-poster-placeholder`,
then send a video of its own (`two-client/video-posters/2-posters.yaml`). The
script passes only when the peer finds a JPEG `pv` in the device's envelope.

**Group membership without timeline lines (automated, #1506):** `mobile/scripts/maestro-membership.sh <device>`
— `pollis-tui`'s `membership_peer` example plays the group's creator, and signs
up a second account that stays a pending invitee. The creator invites the
device's fresh account by email; the device accepts, opens #General and posts —
its own message is tagged "new" and no "joined" line appears — then creates a
group of its own and posts there untagged, since a creator never carries the tag
(`two-client/membership/1-accept-and-post.yaml`). The creator then invites the
pending account and makes the device an admin; the device finds the pending
invite under "Invited" on the member page (invitee, "invited by") and revokes it
(`2-tags-and-revoke.yaml`). The script passes only when the creator has seen the
invite listed and then gone.

**Unread for messages that arrived while away (automated, #1422):** `mobile/scripts/maestro-unread.sh <device>`
— the lost-message shape with `pollis-tui`'s `unread_peer` example, which sends
only when the script touches a signal file. The device opens a DM with a fresh
peer and goes to the background (`two-client/unread/1-open-dm.yaml`); the peer
sends two; on resume the Direct tab badge and the row's count must read 2
(`2-resume.yaml`), then the app is closed; the peer sends a third; after a cold
start and the PIN the count must read 3, opening the DM clears it, and it stays
cleared after another relaunch (`3-cold-start.yaml`).

**A read on the account's other device (automated, #1628):**
`mobile/scripts/maestro-cross-device-unread.sh <device>` — `pollis-tui`'s
`cross_read_peer` example plays the account's first device (`owner`, whose
recovery key enrolls the phone) and a person writing to it (`sender`). With the
Direct tab on screen the sender writes twice (the row reads 2), the owner device
fetches them and its delivered receipts wake the phone (still 2), then the owner
device reads the DM — a read receipt plus a cursor push — and the DS's
`read_cursors_changed` nudge clears the row count and tab badge with no
navigation (`two-client/cross-device-unread/1…5`). On Android it then sends the
app to the background: the sender writes once, the notification arrives, the
owner device reads it, and the tray must empty (#1508) before the app resumes
with nothing unread (`6-resume.yaml`).

**A peer's safety number changes (automated, #1605):** `mobile/scripts/maestro-key-change.sh <device>`
— `pollis-tui`'s `key_change_peer` example, run as two devices of one account.
`first` signs up, accepts the device's DM and sends a message; the device
verifies its safety number and the DM header shows `icon-dm-verified`, with no
key-change line (`two-client/key-change/1-verify.yaml`). `reset` then signs in
to the same account on a new device and recovers it with a NEW account key
(same user id) and opens a new DM. Accepting and opening it must show ONE
`row-key-change-<id>` ("Your safety number with @peer changed. Verify it"),
no verified shield, and the line opens `dm/safety` reading "Mark verified";
after a cold start the line is still there, once (`2-changed.yaml`). The
headless path (Notification Service Extension / FCM) writes the same row
through the same trigger; `simctl push` cannot start an extension, so that path
is pinned by the flows test `rich_push::a_key_change_seen_headless_shows_once_in_the_app`.

**Tab badges for pending requests and invites (automated, #1561):** `mobile/scripts/maestro-pending-badges.sh <device>`
— `pollis-tui`'s `pending_badge_peer` example, run as three fresh accounts.
The device signs up with no badge on either tab
(`two-client/pending-badges/1-signup.yaml`); two strangers then open DMs with
it and a third creates two groups and invites it to both. Without opening
Direct or Groups, `badge-direct` and `badge-groups` must each read 2; accepting
one request drops Direct to 1 and blocking the other sender clears it
(`2-requests.yaml`); declining one invite drops Groups to 1 and accepting the
other clears it (`3-invites.yaml`).

**DM request push (automated, Android, real pushes, #1734):** `mobile/scripts/maestro-dm-request-push.sh <android-device>`
— `pollis-tui`'s `dm_request_peer` example plays two strangers (dev DS running
#1734). Needs a Release build of a #1734 binary against that DS, on an emulator
with Play services. The device signs up, the script grants the notification
permission and the app registers and backgrounds
(`two-client/dm-request-push/1-signup.yaml`, `2-register-and-background.yaml`).
Stranger A's request must leave exactly one notification, "Message request" /
"<A> wants to message you" (read with `dumpsys notification`). A's first message
re-posts it under a new handle and A's second posts nothing (same single
notification, same handle). Tapping it opens `screen-dm-requests`, not the chat,
and the request is accepted (`3-tap-opens-requests.yaml`). Stranger B's request
is then accepted from the request list without tapping its notification, and
the tray must empty (`4-accept-clears.yaml`).

**Store-listing screenshots:** `mobile/scripts/maestro-store-shots.sh <device> [out-dir]`
— `pollis-tui`'s `store_shots_peer` example plays the second person (dev DS
only). The device shoots sign-in, signs up, makes "Design Team" with #General,
invites the peer and opens a DM (`two-client/store-shots/1-setup.yaml`), answers
in both with a quoted reply and a reaction (`2-converse.yaml`), then takes one
clean shot per listing screen (`3-shoot.yaml`, `docs/store-listing.md` §7). The
script puts the status bar in Android demo mode (9:41, full battery, no
notification icons). Play wants at most 2:1, so pad the 1080×2400 shots to
1200×2400 before uploading.

**QR sign-in, phone to phone (automated, #1361):** `mobile/scripts/maestro-qr-link.sh <signed-in-device> <new-device>`
— the NEW device shows its code ("Sign in with another device" → "Show a code
instead", `two-client/qr-link/2-new-show-code.yaml`), the script reads it off
that device's UI tree, the signed-in device scans it from Security → Scan to
sign in a device (entered as text through the camera's own handler: a simulator
camera can't be aimed at another screen) and confirms with its PIN, and the new
device must finish its PIN and list the group — no Approve, no code typed on
it. Needs a dev DS with the v2 link endpoints.

**QR sign-in, phone and desktop (automated, #1361 + #1207):** `mobile/scripts/maestro-qr-scan.sh <device>`
— `enroll_peer` plays the desktop. First the desktop is not signed in and
offers its code (`offer` mode); the phone scans it and the peer must enroll.
Then the other way: the peer signs up and shows a v1 code (`show` mode), the
phone signs in with it as a new device (`v1-1-…`, `v1-2-…`), and the peer
approves.

**Device link (automated):** `mobile/scripts/maestro-device-link.sh <existing-device> <new-device>`
runs both halves of device-linking sign-in across two devices
(`.maestro/two-client/device-link/`): the existing device signs up and makes
a group, the new device signs in and shows its code, the script reads it off
the new device's UI tree, the existing device approves, and the new device
must create its PIN, finalize, and list the group. Any pair works
(iOS sim + Android emulator is the usual one). It exists because device
linking shipped broken on mobile — finalize ran before `set_pin` opened the
local DB — and no flow drove both halves.
Steps 5-10 (#1291) then use the one account on both devices: a message from
each device reaches the other live in the open #General, and one sent while
the new device is backgrounded **and offline** (Android: airplane mode for
~20 s, so its LiveKit wake-up is lost) must land when it returns — via the
resume catch-up alone (`launchApp: stopApp: false`, no relaunch). On the
pre-#1291 build step 10 fails exactly there.

Run a second simulator/emulator with the peer account and drive it with a
parallel Maestro invocation, reusing `subflows/sign-in.yaml` with the peer env:
```bash
maestro --device <peer-udid> test -e MAESTRO_EMAIL=$MAESTRO_PEER_EMAIL \
  -e MAESTRO_OTP=$MAESTRO_OTP -e MAESTRO_PIN=$MAESTRO_PIN \
  .maestro/subflows/sign-in.yaml
```
Then assert convergence on both devices (peer sends → primary sees it live).

## Selecting by text, and the 2026-10 redesign

Every action the flows tap has a `testID` (`group/new`'s submit is
`btn-submit-group`; the redesign removed its Cancel). The flows still match
TEXT in only a few places: the "General" channel row (its dynamic
`row-channel-<id>` isn't known ahead of time), message/search-hit regexes
(`.*hello from maestro.*`), group names in a peer's list, and the iOS
"Open" system prompt. The redesign made all copy sentence case — there are no
uppercased labels any more — and Maestro's text match is a full,
case-sensitive regex, so any new text selector must match the catalogue string
exactly (`frontend/src/i18n/locales/en/*.json`) or use a regex.

Layout changes that flows depend on (see `SELECTORS.md` for the ids):
- **Groups tab:** with no groups it shows an empty state with
  `btn-create-group` / `btn-join-group`; with groups, those two live in the
  "+" sheet (`btn-add-group`) at the end of the group pill strip. Flows tap
  `btn-add-group` with `optional: true` first. A pill (`row-group-<id>`)
  switches the in-place `panel-group`; it does not push `screen-group`.
  Creating a group (`btn-submit-group`) also lands here, with the new group
  selected: wait for `btn-group-menu` with text `"<name>, .*"` (its label is
  "<name>, group menu…") and `panel-group` — there is no back step.
- **iPad (regular width):** Groups/Direct are two-pane; a conversation opens
  in the right pane (`screen-chat`, no `btn-back`) and the pages it opens
  (info, members, settings, thread, profile) push inside that pane. Flows that
  tap `btn-back` to leave a conversation mark it `optional: true`.
- **Android photo picker, several photos:** a tile tap can be reported done
  and select nothing (the grid redrawing under a sync, or a tile for a photo
  MediaStore no longer holds). `subflows/pick-six-photos-android.yaml`
  therefore waits for the grid to be listed and still, taps each tile through
  `pick-android-photo.yaml`, which checks the selection bar's
  "<n> photos or videos selected" after every tap (one retry if it did not
  move), and asserts "6 photos or videos selected" before the caller taps
  Done (#1716).
- **iPad photo picker:** a popover drawn out of process, so its cells, Sort
  and Filter menu and ✓ are not in the hierarchy and text taps find nothing.
  `maestro-run.sh` passes `IPAD=1` on the iPad, and every flow that picks
  photos runs `subflows/ipad-pick-photos.yaml` there (point taps: sort by
  Added, the newest `CELLS`, ✓ when `CONFIRM` is "1") instead of its phone
  steps. `profile-photo` (picker + crop, out of process on every iOS device)
  and `keyboard-recovery`'s bottom toggle and PIN "0" key take per-device points
  the runner also passes (`PICK_POINT` / `USE_POINT`, `SCAN_TOGGLE_POINT`,
  `PIN_ZERO_POINT`); a line in
  `.maestro/.env` overrides them.
- **Direct tab:** DM requests are accepted on `dm/requests`, via
  `row-dm-requests`.
- **Sign out** asks first, in a native Alert with no testID: after
  `btn-sign-out`, tap the label `"Sign out"` (`auth:shell.signOutTitle`; es
  `"Cerrar sesión"`). No flow signs out today.
- **Search** puts its field at the top with the hits right under it, so a
  point tap to dismiss the keyboard can open a hit; `hideKeyboard` works there
  (normal keyboard with a return key). The number pad (OTP / change-email
  codes) has no return key, so `hideKeyboard` cannot close it.

## Known issues found by the first real run (2026-08-22)

The suite had never been executed anywhere before this. Fixed here:

- **Non-idempotent accounts.** Every flow `clearState`s and then signs UP, so a
  single fixed `MAESTRO_EMAIL` only worked on its first use ever — after that
  the account existed, auth took the returning-device enrollment path, no
  create-PIN screen appeared, and the flow died on `screen-auth-pin`. Seven of
  eight flows failed for this one reason. `maestro-run.sh` now mints a fresh
  disposable address per flow and runs each flow as its own invocation.
  (Enrollment needs the recovery key, which the harness cannot hold, so
  re-signing-up is the only self-contained option.)
- **`dms.yaml` had a step that could not fail.** It tapped
  `row-user-${MAESTRO_PEER_HANDLE}`, but the app emits `row-user-<userId>`.
  With `optional: true` the flow walked straight past and "sent" a message into
  whatever screen it was on. Now an id regex prefix, and not optional.
- **`sign-in.yaml` asserted the wrong landing screen.** It waited for
  `screen-groups`; it now waits for the tab bar and each flow taps the tab it
  needs.

Still open, needs app-side triage (not harness bugs):

- **Sign-in lands on the Self tab.** `initializing.tsx:76` replaces to
  `/(tabs)/groups` and nothing in the app routes to `/(tabs)/self`, yet Self is
  what renders. Reproducible.
- **A handle rename does not reach messages you then send.** `sender_username`
  is denormalized onto each message at send time from a cached `currentUser`
  (`hooks/queries/useMessages.ts`), so after changing your handle your own new
  messages still carry the old one — including across an app restart — while
  the conversation header shows the new one.

## Note on authoring vs running

Everything here is static, box-authored. The **running + screenshot capture +
defect triage** is the Mac step — the "visual evaluator later" in the plan.
Expect the first Mac run to need minor `extendedWaitUntil` timeout tweaks and to
close the `testID` gaps above.
