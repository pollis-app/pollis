# Store listing — App Store + Google Play

Everything the owner needs to fill in App Store Connect and the Play Console for the Pollis
mobile app (`mobile/`, bundle/package id `com.pollis.mobile`). Compliance-questionnaire drafts
(privacy manifest, export analysis, Data-safety reasoning) live in
`docs/mobile-compliance-answers.md`; this file is the listing copy plus the answers in
fill-the-form order, and repeats a compliance answer only where a store form asks for it.

**Publisher context:** published by **Daniel Kral, an individual developer** (no corporation, no
D-U-N-S). The seller name shown on both stores will be the personal name — that is how individual
accounts work on both stores, and there is no way around it short of forming an entity.

---

## 1. Copy

### App name

**Pollis** — both stores. (App Store name limit 30; Play title limit 30. Leave the descriptor to
the subtitle/short description rather than keyword-stuffing the name.)

### App Store subtitle (30 chars max)

> `End-to-end encrypted messaging`

Exactly 30 characters.

### Play short description (80 chars max)

> `Private, end-to-end encrypted messaging. Your keys never leave your devices.`

76 characters.

### Full description (both stores; App Store limit 4000, Play limit 4000)

> Pollis is a private messenger for groups and direct messages, built on MLS (Messaging Layer
> Security, RFC 9420) — the IETF standard for end-to-end encrypted group messaging.
>
> Messages, attachments, reactions, and calls are encrypted on your device before anything is
> sent. The servers store and forward only ciphertext they can never read. That is a property of
> the protocol, not a promise in a policy.
>
> WHAT YOU GET
> • Slack-style groups with channels, plus direct messages
> • End-to-end encryption for every message, file, and reaction — no opt-in "secret chat" mode;
>   there is only the encrypted mode
> • Post-quantum-hybrid key agreement and signatures (ML-KEM, ML-DSA) on top of the MLS suite
> • Multi-device: link your phone to your desktop by scanning a QR code
> • Content-free push notifications — the notification never contains your message or its sender
> • Safety numbers to verify who you are talking to, and a public transparency log that makes
>   silent key substitution detectable
>
> WHAT YOU DON'T GET
> • No ads, no tracking, no analytics SDKs — verifiable in the source code, which is public
> • No phone number required — sign in with an email address and a one-time code
> • No server-side message history mining: the operator cannot read message content, full stop
>
> HONEST LIMITS
> Pollis is an open preview of a proof-of-concept, run by an individual developer. Expect rough
> edges in the interface. Message delivery is the part treated as non-negotiable. Messages sent
> before you joined a conversation are cryptographically unreadable to you, and a brand-new
> device starts with no history — that is what end-to-end encryption without key escrow means.
>
> The full privacy policy, the security whitepaper, the subprocessor list, and the source code
> are all public: https://pollis.com

(Play allows the same text; drop the bullets' `•` for `-` if the console complains about
formatting. No emoji, no "#1", no store-badge language — both stores reject those.)

### App Store keywords (100 chars max, comma-separated)

> `encrypted,messenger,private,secure,chat,e2ee,mls,privacy,group,team,dm,message,channels`

87 characters. Do not add competitor trademarks (signal/telegram/whatsapp) — grounds for
rejection under 2.3.7.

### App Store promotional text (170 chars max, editable without review)

> `Open-preview build. End-to-end encrypted groups and DMs — the server only ever sees
> ciphertext. No ads, no tracking, no phone number.`

### Category

- **App Store:** primary **Social Networking**; secondary **Utilities** (optional, or leave
  empty).
- **Play:** category **Communication** (Play has no "Social Networking"; Communication is where
  Signal/Telegram/WhatsApp sit), tags "messaging".

### URLs

| Field | Value |
|---|---|
| Marketing URL | `https://pollis.com` |
| Support URL | `https://pollis.com/faq.html` (support contact: `dankral01@gmail.com`) |
| Privacy Policy URL (both stores) | `https://pollis.com/privacy.html` |
| Terms of Service / EULA | `https://pollis.com/terms.html` (App Store: leave "standard Apple EULA" selected unless you want the custom one; Play: enter the URL) |
| Play account-deletion URL (required — Play asks for a web resource describing deletion) | `https://pollis.com/privacy.html#deletion` |
| Copyright (App Store field) | `© 2026 Daniel Kral` |

---

## 2. Age rating

Position (from `docs/mobile-compliance-answers.md` §4): Pollis is an **unmoderated, E2EE
communication app**. The operator cannot read content and therefore cannot moderate it. Answer
every questionnaire truthfully on that basis and accept whatever tier results — do **not** claim
moderation the product cannot perform.

**App Store Connect:**
- All app-supplied content categories (violence, sexual content, profanity, horror, gambling,
  alcohol/drugs, contests): **None** — the app ships no such content of its own.
- Unrestricted web access: **No** (there is no in-app browser).
- Made for Kids: **No.**
- User-generated content / communication features: **Yes — users can communicate and share
  content, and it is not moderated** (E2EE). Declare the safety controls that exist: block user,
  and safety-number verification.
- Expected result: with content answers all "None" the floor would be 4+, but the
  unmoderated-communication answers push messengers into the mature tier. Comparable apps:
  Telegram and Discord are 17+ on the App Store; Signal and WhatsApp have historically sat at
  12+. Under Apple's revised tier set (4+/9+/13+/16+/18+, rolled out from 2025) expect the
  questionnaire to compute **16+ or 18+**; accept the computed tier. Recommendation: answer
  honestly and let it land at the mature tier rather than gaming the questionnaire down to 4+ —
  an "anonymous-adjacent unmoderated chat rated 4+" is exactly the profile App Review bounces.

**Google Play (IARC questionnaire):**
- Users interact: **Yes.** Users can exchange content that is not moderated: **Yes.**
- Shares location: **No.** Digital purchases: **No** (nothing is sold in-app today).
- No app-supplied violence/sexual/gambling content.
- Expected result: IARC typically emits **Everyone / PEGI 3 with the "Users Interact"
  interactive element** for messengers answered this way (that is what Signal and WhatsApp
  carry); some regions may compute Teen. Accept the computed set.

---

## 3. App Privacy nutrition label (App Store Connect → App Privacy)

Must match `mobile/PrivacyInfo.xcprivacy` / `app.json` `privacyManifests` exactly — Apple
cross-checks the manifest against the label. Declared there: Email Address, User ID, Name; no
tracking.

**Do you collect data from this app? → Yes.**

| Label item | Answer | Detail |
|---|---|---|
| Contact Info → Email Address | **Collected** | Linked to identity: **Yes**. Used for tracking: **No**. Purpose: **App Functionality** (sign-in identifier, OTP delivery). |
| Contact Info → Name | **Collected** | Optional display name. Linked: **Yes**. Tracking: **No**. Purpose: **App Functionality**. |
| Identifiers → User ID | **Collected** | Username/handle + app-generated account and device ids (device ids are covered here, matching the manifest — no separate Device ID declaration). Linked: **Yes**. Tracking: **No**. Purpose: **App Functionality**. |
| Everything else (location, contacts, messages*, photos*, health, financial, browsing/search history, purchases, diagnostics, advertising data, "Other") | **Not collected** | No analytics/crash/ads SDK exists in the app. |

\* **Judgment call, stated openly:** Apple defines "collected" as transmitted off-device *and
accessible to the developer*. Message content and attachments are E2EE ciphertext the developer
cannot access, so they are **not declared** — the same posture Signal takes for message bodies.
Keep this consistent with the privacy manifest (which also omits them). If App Review pushes
back, the fallback is to declare Messages/Photos as collected, linked, App-Functionality-only,
and note E2EE in the review notes.

**Tracking section:** "Do you or your third-party partners use data for tracking?" → **No.**
(`NSPrivacyTracking=false`, no tracking domains, no ATT prompt needed.)

**Also required by App Review (separate from the label):** in-app **account deletion**. The
delete-account flow exists in `pollis-core` (`delete_account`) and desktop's Security page, but
**the mobile app has no delete-account UI yet — this is a submission blocker for both stores**
(Apple guideline 5.1.1(v); Play's account-deletion policy). Wire a mobile Settings → Security →
Delete account path before submitting, or expect rejection.

---

## 4. Play Data safety form (Play Console → App content → Data safety)

Full reasoning in `docs/mobile-compliance-answers.md` §3. Form answers:

- Does your app collect or share required user data types? → **Collects: Yes. Shares: No.**
  (Turso, Cloudflare, Resend, Expo/APNs/FCM, and the LiveKit host are service providers acting
  on the developer's behalf — that is "collection", not "sharing", under Play's definitions.)
- Is all user data encrypted in transit? → **Yes** (TLS everywhere; message content additionally
  E2EE).
- Do you provide a way for users to request data deletion? → **Yes** — in-app account deletion
  (see the blocker note in §3 above: must exist on mobile before answering this) plus
  `https://pollis.com/privacy.html#deletion`.

Per-type declarations (each: **collected**, **not shared**, **not processed ephemerally**,
**required** unless noted, purpose **App functionality** — plus **Account management** for email
and user IDs; none used for ads/marketing):

| Play data type | Collected | Notes |
|---|---|---|
| Personal info → Email address | Yes | Required sign-in identifier. |
| Personal info → User IDs | Yes | Username + account/device ids. |
| Personal info → Name | Yes (optional) | Display name; user can decline (leave empty). |
| Messages → In-app messages | Yes | Transmitted as **E2EE ciphertext**; developer cannot read. Play counts "transmitted off device" as collected, so declare it — do not claim it is not transmitted. |
| Photos and videos / Files and docs | Yes | Only media the user deliberately sends; E2EE blobs. |
| Device or other IDs | Yes | Push token + app-generated device id (content-free push). |
| Location, Contacts, Calendar, Financial info, Health & fitness, Web browsing history, Search history, Installed apps, Audio (as a collected category), App activity/diagnostics (analytics) | **No** | No such collection; no analytics/crash SDK. |

---

## 5. Export compliance (encryption)

Engineering reading, not legal advice — see the caveat in `docs/mobile-compliance-answers.md` §2.
The owner is the exporter of record.

- **Does the app use encryption? YES.** Standard, published algorithms only: MLS (RFC 9420 —
  ChaCha20-Poly1305, HKDF-SHA-384, X25519 + ML-KEM-768 hybrid key agreement, ML-DSA-44
  signatures), AES-256-GCM for attachments, Ed25519. Nothing proprietary or non-standard.
- **Apple's `ITSAppUsesNonExemptEncryption`: `YES` (true).** E2EE messaging is well past the
  HTTPS/authentication-only exemptions, so "No" would be false. Set it in `mobile/app.json` →
  `expo.ios.infoPlist` once the owner is ready to stand behind the filing status (deliberately
  not committed yet — see mobile-compliance-answers §2.1).
- **Classification:** mass-market software using standard crypto → self-classified
  **ECCN 5D992.c** via the **Cryptography Note (Note 3 to Category 5, Part 2)** — generally
  available to the public, installed by the user without supplier support, standard crypto —
  exportable under **License Exception ENC, EAR §740.17(b)(1)**. (Note 4 — "crypto is ancillary
  to primary function" — does **not** fit a messenger whose core feature is encryption; the
  mass-market route is the right one, not the Note 4 decontrol.) No CCATS classification
  request is needed for this route.
- **US BIS annual self-classification report:** for products self-classified 5x992.c under
  §740.17(b)(1), email the self-classification report (the CSV format of Supplement No. 8 to
  EAR Part 742: product name, ECCN 5D992.c, authorization type, item description, manufacturer)
  to **`crypt@bis.doc.gov`** and **`enc@nsa.gov`**, **due February 1 each year**, covering
  products first exported in the prior calendar year. Put it on a recurring calendar.
- **France:** France additionally requires a **declaration to ANSSI** for supplying/importing
  means of cryptology (dossier per Article 29, Law 2004-575 / décret 2007-663). App Store
  Connect asks whether the app will be available in France and whether the French declaration
  has been made. Either file the ANSSI declaration (free, one-time, form on ssi.gouv.fr) before
  including France, or deselect France from availability until it is filed.
- Keep the self-classification record + ANSSI receipt to hand to Apple/Google if asked.

---

## 6. Google Play — new personal developer account requirements

Personal (non-organization) Play developer accounts created after 13 Nov 2023 must, before
production access is granted:

- Run a **closed test with at least 12 testers** (Google reduced this from the original 20
  during 2025) who have been **opted in continuously for the last 14 days** before applying for
  production.
- Complete identity/device verification on the account.

Practical plan: create the closed track, enroll 12+ real humans (email-list track), keep them
opted in for 14+ days of actual use, then apply for production and answer Google's
"about your test" questionnaire. Verify the current numbers in the Play Console at submission
time — Google has changed them once already.

Apple has no equivalent gate; TestFlight is optional but recommended before review.

---

## 7. Screenshots

### Required sizes

**App Store (per-locale, up to 10 each):**

| Device class | Size (portrait) | Required? |
|---|---|---|
| iPhone 6.9" (16/17 Pro Max class) | **1320 × 2868** (or 1290 × 2796) | **Yes** — the one mandatory iPhone set; Apple scales it down for smaller devices. |
| iPhone 6.5" (legacy 6.5"/6.7" devices) | 1242 × 2688 or 1284 × 2778 | Optional if the 6.9" set is provided; upload only if you want pixel-perfect framing on older phones. |
| iPad 13" | **2064 × 2752** (or 2048 × 2732) | **Yes — required while `mobile/app.json` keeps `ios.supportsTablet: true`.** (Alternative: set `supportsTablet: false` and skip iPad entirely; the app is phone-designed, so consider this seriously.) |

PNG or JPEG, no alpha. The app is portrait-locked on iPhone, so shoot portrait.

**Google Play:**

| Asset | Spec | Required? |
|---|---|---|
| Phone screenshots | 2–8, JPEG/24-bit PNG, 16:9 or 9:16, each side 320–3840 px (use 1080 × 1920 or larger) | **Yes** (min 2) |
| 7" tablet screenshots | up to 8, same constraints (≈1200 × 1920) | Required for the "designed for tablets" listing quality; skip only if not targeting tablets |
| 10" tablet screenshots | up to 8, same constraints (≈1600 × 2560) | Same as above |
| Feature graphic | **1024 × 500** JPEG/PNG | **Yes** |
| App icon | 512 × 512 32-bit PNG | **Yes** (also in the console, independent of the binary) |

### Shot list (5 screens)

Capture on a real device or simulator in the dark amber theme; no synthetic device frames
needed (both stores accept raw screens). Use obviously-fake conversation data — no real emails,
names, or codes.

1. **Groups tab** — the channel/group list. Caption: "Groups and channels, end-to-end
   encrypted."
2. **A channel conversation** — messages with reactions and the composer. Caption: "The server
   only ever sees ciphertext."
3. **Direct messages** — a DM thread. Caption: "Private DMs. No phone number required."
4. **Security screen** — device list + safety-number/key verification (the E2EE proof point;
   `mobile/app/self/security.tsx`). Caption: "Verify who you're talking to."
5. **Sign-in flow** — the email → one-time-code screen. Caption: "Sign in with an email and a
   one-time code. Your keys never leave your devices."

For iPad/tablet shots, reuse screens 1, 2, and 4 (iPad rotates freely per `app.json`, so
landscape is acceptable there).

---

## 8. Pre-submission blockers (recap)

1. **Mobile in-app account deletion UI** — required by both stores; core command exists, mobile
   UI does not (§3).
2. **`ITSAppUsesNonExemptEncryption` in `app.json`** — set to `true` when the owner adopts the
   export position (§5).
3. **BIS annual report + ANSSI France declaration** — file (or restrict France) before release
   (§5).
4. **SystemBootTime / `@livekit/react-native-webrtc` linkage check** at EAS build time — see
   `docs/mobile-compliance-answers.md` §1; an undeclared required-reason API is an automatic
   rejection (ITMS-91053).
5. **Play closed-testing gate** — 12 testers / 14 days before production (§6).
6. **EAS credentials** (`eas init`, APNs key, FCM service account) — push is content-free but
   still needs real credentials (#344).
