# E2E happy-path scenario inventory (1 & 2 users)

Sorted by **value × (1 − confidence)** — highest-value, least-confident (most
worth an automated test) first. "Confidence" = how sure static code analysis
makes me that the flow already works. Status: ✅ covered by an existing script,
🟡 new script added here, ⬜ not yet automated.

## Legend
- **V** = product value if it breaks (5 = core promise, 1 = cosmetic)
- **C** = static-analysis confidence it works today (5 = certain, 1 = unknown)
- **Priority** = V high, C low → test first

## Two-user scenarios

| # | Scenario | V | C | Status | Notes |
|---|----------|---|---|--------|-------|
| M-CH | Group + text-channel convergence: A creates group+channel, invites B, B accepts, A posts, B receives | 5 | 2 | 🟡 | Core Slack model; entirely untested cross-client. Invite path is MLS-synchronous (reliable). |
| M-VC | Voice channel: A joins, B joins same channel, both see 2 participants, A leaves, B sees drop | 5 | 2 | 🟡 | Explicitly requested. Needs LiveKit + audio + group membership (MLS voice key). |
| M-OFF | Offline convergence: A sends while B's app closed, B relaunches → receives (bounded history) | 5 | 2 | ⬜ | "Messages must work" invariant. |
| M-DM2 | DM bidirectional: B replies to A, A sees B's message | 4 | 3 | 🟡 | Reverse direction never asserted (existing test is A→B only). |
| M-JR | Join-request path: B searches group by slug, requests, A approves, B becomes member | 4 | 2 | ⬜ | Alt to invite; relies on async realtime membership_changed → welcome poll. |
| M-EDIT | Edit convergence: A edits a message, B sees new text + (edited) | 3 | 2 | ⬜ | |
| M-DEL | Delete convergence: A deletes, B sees `[deleted]` | 3 | 2 | ⬜ | Content tombstoned, row stays. |
| M-ORD | Ordering: A sends 3 messages, B receives all 3 in order | 4 | 3 | ⬜ | |
| M-PRES | Presence: A sees B come online / go offline | 3 | 2 | ⬜ | LiveKit realtime dependent. |
| M-CALL | 1:1 call place + accept, both see each other | 5 | 3 | ✅ | two-client-call.js |
| M-CAM | Call + camera: A camera on, B sees remote tile | 4 | 3 | ✅ | two-client-camera.js |
| M-SS | Call + screenshare: A shares, B sees remote tile | 4 | 3 | ✅ | two-client-screenshare.js |
| M-DECL | Call decline: A calls, B declines, A sees ended | 3 | 2 | ⬜ | |
| M-HANG | Call hangup convergence: A hangs up, B sees end | 3 | 3 | ⬜ | |
| M-KICK | A kicks B from group, B loses access | 3 | 2 | ⬜ | |
| M-LEAVE | B leaves group, A sees membership drop | 2 | 2 | ⬜ | |
| M-CONV | DM request → accept → message (base) | 5 | 4 | ✅ | two-client.js |

## Single-user scenarios

| # | Scenario | V | C | Status | Notes |
|---|----------|---|---|--------|-------|
| S-RESTART | Restart persistence: sign up, kill app, relaunch same data dir → data/session survives | 5 | 2 | 🟡 | "Same device keeps data" invariant. Reveals real cold-start behaviour. |
| S-RELOGIN | Logout (keep data) → re-login (email+OTP+PIN) → app-ready | 4 | 3 | ⬜ | Confirmed: always re-auth, never PIN-only. |
| S-EDIT | Send → edit own message (Enter to save) → shows (edited) | 3 | 3 | ⬜ | No submit testid; save via Enter keydown. |
| S-DEL | Send → delete own message → `[deleted]` | 3 | 3 | ⬜ | |
| S-GRP | Create a group | 3 | 3 | ⬜ | Covered as a step of M-CH. |
| S-TXTCH | Create a text channel | 3 | 3 | ⬜ | Covered as a step of M-CH. |
| S-VOXCH | Create a voice channel (toggle type switch) | 3 | 3 | ⬜ | Covered as a step of M-VC. |
| S-NOMIC | Voice join with no capture device → joins listen-only (not blocked), tray shows "listening only" | 5 | 3 | ✅ | voice-channel-no-mic.js. Forces the path with POLLIS_DISABLE_MIC=1. |
| S-RENAME | Rename a channel / group | 2 | 3 | ⬜ | |
| S-DELCH | Delete a channel | 2 | 3 | ⬜ | |
| S-SIGNUP | Full signup: email→OTP→secret key→PIN→app-ready | 5 | 4 | ✅ | e2e.js |
| S-BADOTP | Wrong OTP rejected inline | 4 | 4 | ✅ | invalid-otp.js |
| S-SMOKE | App launches, login screen renders | 5 | 5 | ✅ | smoke.js |

## Fixture bugs found while getting the suite green locally
Two blockers stopped **every** two-client script from passing on a local dev box
(they only ever passed in CI, where `.env.development` is absent). Both fixed on
this branch:

1. **Missing commit-log migrations.** `start-backend.sh` runs the DS in single-DB
   fallback (`LOG_DB_*` unset) so the MLS control-plane tables share the one
   libsql DB, but only `migrations/` was applied — never `migrations-log/`. The
   `mls_welcome` UNIQUE dedupe index (#430) and `mls_commit_since` table (#539)
   were missing, so the DS's idempotent Welcome upsert failed with "ON CONFLICT
   clause does not match any PRIMARY KEY or UNIQUE constraint" and Welcomes never
   persisted → no cross-client MLS delivery. Fix: `apply-log-migrations.py`,
   invoked from `start-backend.sh`.

2. **`.env.development` leaked prod URLs into the app.** `appEnvFor` merged
   `{...process.env, ...devEnv}`, so `.env.development`'s
   `LIVEKIT_URL="wss://rtc.pollis.com"` overrode the local fixture's
   `ws://127.0.0.1:7880`. The app dialed **production** LiveKit, which rejected
   the locally-minted devkey token (401 "invalid authorization token") → realtime
   dead → the `membership_changed` hint that triggers `poll_mls_welcomes` never
   arrived → nothing converged. Fix: flip the merge to `{...devEnv,
   ...process.env}` so the fixture wins (CI unaffected — no `.env.development`
   there).

## Not testable as-is
- **Reactions** (add/remove, convergence): `MessageReactions` is commented out in
  both skins of `MessageItem.tsx` — the `reaction-*` testids don't exist in the
  running app. Would need the component re-enabled first.
</content>
</invoke>
