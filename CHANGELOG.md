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

Security release.

- Closed all four high-severity findings from an internal security audit.
- **Message deletion is now scoped to the conversation you are authorised in.**
  Previously a delete request was not checked against the conversation it named.

## v1.9.3 — 2026-08-13

- Fixed video in the call stage being clipped at small window sizes.

> **Reproducibility note.** The independent rebuild of this release did not
> match the published payload, and that has not been explained. It is recorded
> as a genuine finding at <https://pollis.com/assurance> rather than written
> off. Every release since has reproduced bit-for-bit.

## v1.9.2 — 2026-08-13

- Fixed release provenance by pinning the `cosign` installer to an exact
  version — the moving major tag it had been using does not exist.

## v1.9.1 — 2026-08-13

- **Thread isolation fixes**, plus skin-aware panels and contextual details.
- **Pseudonymous LiveKit room names**, so the media server can no longer map a
  room to a conversation.
- Fixed a case where the client could pick an external join with no group state
  to join onto.
- Cut input lag in the terminal client on Linux.

## v1.9.0 — 2026-08-12

- **Slack-style threads in channels.**
- **A right-hand context panel** showing members and shared media.
- **Relay overlay v1**: multi-hop onion routing, peer-hosted relays, live
  revocation, and anchoring in the transparency log. Opt-in and off by default —
  see <https://pollis.com/subprocessors> for who operates the pool today and
  what it does and does not hide.
- Calls: TURN over 443 for restrictive networks, and a wider relay port range.
- Arch packaging: the desktop entry now pins an absolute path, so an unrelated
  binary earlier on your `PATH` can no longer hijack the launcher.

---

## Earlier releases

Versions before v1.9.0 predate this file. Their contents are in the
[GitHub releases](https://github.com/actuallydan/pollis/releases), which carry
the auto-generated pull-request list, and every one of them has its hashes in
the public transparency log regardless of how its notes were written.

## Keeping this file honest

Entries are written when a release is cut, from the pull requests merged since
the previous tag. Two rules:

1. **Describe the change from the user's side.** "Message deletion is now scoped
   to the conversation you are authorised in" is useful; "fix(delivery): scope
   message deletes" is not.
2. **A release whose independent rebuild did not reproduce says so**, in the
   entry, with a link to the verdict. A changelog that only records good news is
   marketing.
