# Mobile store-compliance answer drafts (WS4 / #708 / PL-20)

**Status: DRAFTS for the owner to file. This repository cannot file any of these.**
Everything below is engineering's best reading of the questionnaires so the owner has an
accurate starting point to transcribe into App Store Connect, Google Play Console, and (for
export compliance) the U.S. Bureau of Industry and Security (BIS) / NSA. The *in-repo* half of
this ticket — `mobile/PrivacyInfo.xcprivacy` + the `expo.ios.privacyManifests` wiring in
`app.json` — is done and lives alongside this file. The store forms and the export filing need
accounts and a legal declaration that only the owner holds (WS4/PL-17 registers those accounts).

> **Export-control caveat.** The encryption / EAR analysis below is an engineering reading, not
> legal advice. U.S. export law carries real penalties. Confirm the ECCN, the license exception,
> and the filing cadence against current BIS guidance (and/or counsel) before submitting. The
> *exporter of record* (the owner / the entity that registers the Apple + Google accounts) is
> responsible for the filing.

---

## 0. What data Pollis actually collects (the source of truth for every form)

This section was derived directly from the remote schema
(`pollis-schema/migrations/000000_baseline.sql`, the `users` table) and the mobile sign-in /
profile code, then reconciled against **`docs/metadata-retention-policy.md`** (PL-14, #702), which
landed while this ticket was in flight. That policy is the authoritative account of *what is kept
and for how long*; this section is the mobile-collection view of the same facts. Where the two ever
disagree, the retention policy wins and this table is the bug.

Server-visible (Turso, plaintext) account/directory metadata, all **linked to the user**:

| Data | Where | Collected on mobile? | Evidence |
|---|---|---|---|
| **Email address** | `users.email` (NOT NULL UNIQUE) | **Yes** — the sole sign-in identifier (email → OTP → PIN). | `pollis-core/src/commands/auth.rs` `verify_otp`; `mobile/app/(auth)/email.tsx` |
| **Username / handle** | `users.username` (NOT NULL UNIQUE) | **Yes** — account handle, editable in Self → user-settings; directory-searchable. | `pollis-core/src/commands/user.rs`; `mobile/app/self/user-settings.tsx`, `mobile/app/dm/new.tsx` |
| **Display / preferred name** | `users.preferred_name` (nullable) | **Yes, optional** — user-set profile name. | migration `000001_user_preferred_name.sql`; `mobile/app/self/user-settings.tsx` |
| Phone number | `users.phone` (nullable) | **No** — column exists but the mobile app has no phone input and never writes it. | schema only; no mobile UI |
| Avatar | `users.avatar_url` | Optional — points to a **public, unencrypted** R2 object (profile decoration, not message content; anyone with the URL can fetch it). | `pollis-core/src/commands/r2.rs` "Public objects (avatars, group icons)" |
| Message content, attachments, reactions | `message_envelope.ciphertext`, R2 blobs | Transmitted as **E2EE ciphertext only** — the server/developer **cannot** read plaintext (MLS, RFC 9420). | `docs/security-whitepaper.md`; `docs/metadata-minimization-design.md` |
| Push token | `push_token` table | **Yes** — device push token, best-effort, for content-free notifications. Payload carries a fixed title/body plus `{conversationId, kind}` — never a sender or a preview. Retained until the device re-registers or the account is deleted; **no expiry sweep** for uninstalled apps. | migration `000006_push_token.sql`; `mobile/lib/push/`; retention policy §4 |
| Device id / device name | `user_device` | **Yes** — app-generated per-device id + optional device name (device management). | schema; `mobile/app/self/security.tsx` |
| Source IP | **not stored anywhere** | Read for rate-limit keying only (`CF-Connecting-IP` → in-process counter, pruned, not persisted, does not survive a restart). Cloudflare as our edge necessarily sees it, as does any transit provider. | `pollis-delivery/src/ratelimit.rs`; retention policy §3 |

**We do NOT collect:** precise/coarse location, contacts, calendar, photos library (beyond media
the user deliberately sends), health/fitness, financial info, browsing history, search history,
advertising identifiers, or any analytics/crash/telemetry (no analytics or crash SDK is linked).
**No tracking. No data sold or shared for advertising. No third-party ad SDKs.**

Third parties that receive data act as **service providers / processors**, not independent
controllers: Turso (encrypted envelopes + directory metadata), Cloudflare R2 (encrypted blobs),
LiveKit (E2EE-opaque realtime signalling / wake-ups), Expo + APNs/FCM (content-free push).

---

## 1. iOS privacy manifest — `PrivacyInfo.xcprivacy` (DONE in-repo)

Delivered as `mobile/PrivacyInfo.xcprivacy` and wired via `expo.ios.privacyManifests` in
`mobile/app.json`. `expo prebuild` renders that block into `ios/mobile/PrivacyInfo.xcprivacy` and
links it into the app target, so an EAS/prebuild iOS build includes it automatically (`ios/` is
gitignored / regenerated, which is why the wiring lives in `app.json` rather than a committed
`ios/` file; the standalone `.xcprivacy` is the reviewable mirror).

**Collected data types declared:** Email Address, User ID (username + account/device ids), Name
(optional display name) — each **linked = true**, **tracking = false**, purpose **App
Functionality** only. Message content is deliberately **not** declared as collected: it is E2EE
and the developer cannot access it (same posture as Signal/WhatsApp for message bodies).

**Tracking:** `NSPrivacyTracking = false`, `NSPrivacyTrackingDomains = []` (we do not track).

**Required-reason APIs — audited against the actual installed dependency tree**
(`mobile/node_modules`, Expo SDK 55 / RN 0.83.6), not guessed:

Third-party SDKs ship their **own** `PrivacyInfo.xcprivacy`, and Apple unions every linked
manifest, so we do **not** re-declare their usage. Evidence (files present in the tree):

| Required-reason API | Reason code(s) | Declared by (self-covered) |
|---|---|---|
| `…CategoryFileTimestamp` | `C617.1` | `react-native/React/Resources/PrivacyInfo.xcprivacy`, `expo-application` |
| `…CategoryFileTimestamp` | `0A2A.1`, `3B52.1` | `expo-file-system/ios/PrivacyInfo.xcprivacy` |
| `…CategoryUserDefaults` | `CA92.1` | `react-native` core, `expo-constants`, `expo-notifications` |
| `…CategoryDiskSpace` | `E174.1`, `85F4.1` | `expo-file-system` |

**What the app-level manifest adds on top** (first-party code that ships no manifest of its own):

- **`NSPrivacyAccessedAPICategoryFileTimestamp`, reason `C617.1`.** The `pollis-native` Rust
  turbo-module statically links SQLite/SQLCipher (`rusqlite` with `bundled-sqlcipher`, see
  `pollis-core/Cargo.toml`). SQLite's Unix VFS `stat`/`fstat`/`lstat`s the local database + the
  file-backed keystore, both inside the **app container** — exactly what `C617.1` covers. This is
  the one required-reason API our own binary contributes that no pod declares, so it is declared
  at app level.

**Deliberately NOT declared:**

- `…CategoryUserDefaults` and `…CategoryDiskSpace` — used only by the RN/Expo pods above, which
  self-declare. Our Rust code uses neither (`NSUserDefaults` is never called; SQLite does not
  `statfs` for free space). Re-declaring them at app level would be over-broad.

- **`NSPrivacyAccessedAPICategorySystemBootTime` — UNVERIFIED, intentionally omitted for now.**
  The audit found exactly one reference to `mach_absolute_time` in the installed tree:
  `node_modules/@livekit/react-native-webrtc/ios/RCTWebRTC/ScreenCapturer.m`. That package ships
  **no** `PrivacyInfo.xcprivacy`. Whether it is linked into the shipping binary is unsettled:
  voice/video and screen-share are excluded by product decision (#343), and per `mobile/CLAUDE.md`
  the WebRTC Expo config plugins are deliberately absent, so the intended build does not activate
  it — but RN autolinking may still pull the pod in unless it is explicitly excluded. **Action for
  the owner / the EAS build (PL-18):**
  1. Confirm whether `@livekit/react-native-webrtc` is autolinked into the iOS target
     (`npx expo prebuild -p ios` then inspect `ios/Podfile.lock` / the Pods project).
  2. If it is **not** linked → no change; SystemBootTime stays out.
  3. If it **is** linked → either exclude it until voice lands under #343 (preferred, matches the
     "not activated" stance), **or** add `NSPrivacyAccessedAPICategorySystemBootTime` with reason
     `8FFB.1` to `app.json` `privacyManifests` (and this file). A used-but-undeclared API is an
     App Store rejection (ITMS-91053), so this must be resolved at build time, not guessed now.

**Validation:** the plist was parsed with Python `plistlib` (`plutil` is macOS-only; `xmllint`
is not installed on the build box) — parses clean, all keys present. `app.json` re-validated as
JSON.

---

## 2. iOS export compliance — the non-trivial part

Pollis provides **end-to-end encryption** (MLS / RFC 9420: ChaCha20-Poly1305, HKDF-SHA-384, X25519 + ML-KEM-768, ML-DSA-44; AES-256-GCM for attachments,
Ed25519, plus a PQ-hybrid layer using standardized ML-KEM / ML-DSA — see `docs/pq-hybrid-mls-design.md`,
migration `000011_device_pq_signature_pub.sql`). This is **not** the trivial "No" path that
HTTPS-only apps take.

### 2.1 `ITSAppUsesNonExemptEncryption`

**Draft value: `YES` (true).**

Reasoning: Apple lets you answer *No* only if your app's encryption is limited to a short list of
exemptions — calls to Apple's OS crypto, HTTPS/TLS, authentication-only use, or copyright
protection. Pollis's encryption goes well past that: it is a core **E2EE messaging** feature.
Therefore it uses *non-exempt* encryption and the honest answer is **Yes**.

Because the answer is Yes, App Store Connect will require export-compliance information on every
version *unless* the key is set in the build. **Recommended wiring (owner's call — not committed
here because it asserts the owner's legal position):** once the BIS self-classification below is
in hand, set it in `app.json` so the question is answered at build time:

```jsonc
// mobile/app.json → expo.ios.infoPlist
"ITSAppUsesNonExemptEncryption": true
```

It is left **unset** in-repo deliberately: setting it is the owner's declaration to make, and it
should track the actual BIS filing status.

### 2.2 Which EAR exemption / classification applies

Pollis uses only **standard, published cryptographic algorithms** and is **publicly available
(mass market)**. That makes it eligible for **License Exception ENC under EAR §740.17(b)(1)**,
self-classified as **ECCN 5D992.c** (mass-market encryption software). It does **not** need the
heavier §740.17(b)(2)/(b)(3) classification-request (CCATS) path, which is for non-standard crypto
or network-infrastructure-grade items.

Apple's follow-up question — *"Does your app qualify for any of the exemptions?"* — the practical
answer is that it qualifies for the **mass-market self-classification** route, so the owner will
either:
- upload the **year-based export-compliance documentation** (the self-classification), or
- provide a **CCATS / ERN** number if the owner chooses/needs the registration route.

### 2.3 What the owner must actually file with the U.S. government (repo cannot do this)

1. **Encryption registration (one-time), if applicable** — an encryption registration in BIS's
   SNAP-R system (Supplement No. 5 to EAR Part 742) yields an **ERN** (Encryption Registration
   Number). Confirm current applicability for a pure 5D992.c mass-market self-classification —
   the requirement has shifted over time; verify against current §742.15 before relying on it.
2. **Annual self-classification report (recurring)** — for items self-classified under
   §740.17(b)(1) as 5A992.c / 5D992.c, email a self-classification report to **BIS**
   (`crypt@bis.doc.gov`) and the **NSA** (`enc@nsa.gov`) **by February 1 each year**, covering
   products first exported/self-classified in the prior calendar year (Supplement No. 8 to EAR
   Part 742). Contents: product name, model/app identifier, ECCN (5D992.c), a short description of
   the encryption (E2EE messenger; standard AES-GCM/ECDH/Ed25519/HKDF/SHA-2 + ML-KEM/ML-DSA),
   author, and availability (public App Store / Play).
3. **Keep the CCATS/ERN/self-classification record** to hand to Apple (and Google, if asked) as
   export-compliance documentation.

**Bottom line for the owner:** answer Apple's encryption question **Yes**, take the mass-market
**5D992.c self-classification** route, and put the **annual Feb-1 BIS/NSA report** on a recurring
calendar. This repo can state the position; only the owner can file it.

---

## 3. Google Play "Data safety" answers (draft)

Complete in Play Console → *App content → Data safety*. Draft, matching §0:

**Does your app collect or share any of the required user data types?** — **Yes** (collect), **No
sharing** with third parties (Turso / R2 / LiveKit / Expo are processors acting on Pollis's behalf,
not independent recipients — confirm each is configured as a service provider).

**Data types collected** (all: *collected*, **not shared**, **linked to the user**, **not** used
for ads/marketing, **cannot** be requested-to-not-collect except by not using the feature):

| Play data type | Category | Purpose | Notes |
|---|---|---|---|
| Email address | Personal info | Account management, App functionality | Required sign-in identifier. |
| User IDs | Personal info | Account management, App functionality | Username/handle + account/device ids. |
| Name | Personal info | App functionality | Optional display name. |
| Messages (in-app) | Messages | App functionality | **E2EE** — transmitted as ciphertext; developer cannot read. Mark *encrypted in transit*; note E2EE. |
| Photos / videos, Files & docs | Photos and videos / Files and docs | App functionality | Only media the user sends; **E2EE** encrypted blobs. |
| Device or other IDs | Device or other IDs | App functionality (notifications) | Push token + app-generated device id; content-free push only. |

**Security section:**
- **Is your data encrypted in transit?** — **Yes** (TLS everywhere; message content additionally
  **end-to-end encrypted**).
- **Can users request that their data be deleted?** — **Yes** — provide the in-app account-deletion
  path / contact and the deletion URL Play requires. (Confirm the account-deletion flow is wired
  and reachable before answering Yes.)
- **Committed to Play Families policy?** — Not a Families/children app.

**Data types NOT collected** (explicitly answer No): location, contacts, calendar, financial info,
health & fitness, web browsing history, search history, installed apps, audio recordings, and any
analytics/crash/diagnostics.

> **Owner judgment calls to confirm before filing:**
> - Whether to mark Messages / Photos / Files as *collected* at all, given they are E2EE and the
>   developer cannot access them. Google's Data-safety guidance treats "transmitted off device" as
>   collection but recognizes E2EE; the conservative, defensible choice is to declare them
>   *collected + encrypted-in-transit + E2EE, not shared, not for ads*. Do **not** claim they are
>   not transmitted.
> - Confirm each processor (Turso, R2, LiveKit, Expo) is contractually a service provider so
>   "sharing = No" holds.

---

## 4. Age rating answers (draft)

Pollis is an **unmoderated, end-to-end-encrypted communication app**: because content is E2EE, the
operator **cannot** moderate message content by design. Answer the questionnaires honestly on that
basis — do **not** claim content moderation the product cannot perform. This drives the rating
upward (comparable apps: Signal 17+, Discord 17+, Telegram 17+ on the App Store).

**Apple (App Store Connect age-rating questionnaire):**
- Made for Kids: **No.**
- Cartoon/realistic violence, sexual content, nudity, profanity, horror, alcohol/drugs/tobacco,
  gambling, contests: **None** (the *app* contains none of these; it ships no such content).
- **Unrestricted web access:** **No** — there is no in-app browser.
- **User-generated content / the app enables communication or sharing of user-generated content:**
  **Yes** — and it is **not moderated** (E2EE). Answer the UGC/messaging questions truthfully;
  expect the result to land at the mature tier (historically **17+**; map to Apple's current
  2025 tier — 13+/16+/18+ — as the questionnaire dictates).
- Provide standard **safety/UGC controls** you *do* have (block user, report — confirm what exists:
  `user_block` / safety-number verification) in the questionnaire where asked.

**Google Play (IARC questionnaire):**
- Category: **Communication / Social**.
- Complete the IARC questionnaire honestly: **users interact**, **user-generated content is
  shared and unmoderated**, **unrestricted internet access**. No app-supplied violence/sexual/
  gambling content.
- Declare the **interactive elements**: "Users Interact," "Shares Location" — **No** (we don't
  share location), "Shares Info" — as applicable (users can share account handle), "Digital
  Purchases" — as applicable to the supporter/community tier if/when it ships.
- IARC will emit regional ratings (ESRB / PEGI / USK / etc.) automatically from the answers.

> **Owner action:** the exact resulting tier depends on the live questionnaire wording (Apple
> revised its tiers in 2025). File honestly per the above inputs; do not overstate moderation.

---

## 5. Checklist — repo vs. owner

| Item | Owner / Store / BIS | This repo |
|---|---|---|
| `PrivacyInfo.xcprivacy` + `app.json` wiring | — | **DONE** |
| SystemBootTime decision (WebRTC linkage) | verify at EAS build (PL-18) | flagged UNVERIFIED above |
| `ITSAppUsesNonExemptEncryption` value | **owner sets** once BIS filing started | drafted (§2.1) |
| BIS/NSA export self-classification + annual report | **owner files** | drafted (§2.3) |
| App Store "App Privacy" nutrition label | **owner files** | drafted (§1, §0) |
| Play "Data safety" form | **owner files** | drafted (§3) |
| Age ratings (Apple + IARC) | **owner files** | drafted (§4) |
| Developer accounts / bundle id / D-U-N-S | **owner** (PL-17) | — |
| Privacy-policy URL + legal text | **owner** (out of scope for #708) | — |
