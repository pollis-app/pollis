# Two-client / special flows — scaffolds

Some parity flows can't be driven by a single Maestro session and are scaffolded
here rather than fully authored, because this box can't run Maestro to shake them
out. They need the **two-device harness** (README "Two-client flows") or platform
state a simulator can't easily fake. Author/verify these on the Mac:

- **dms.yaml** — the initiator side IS authored; the peer runs `subflows/sign-in.yaml`
  (with `-e MAESTRO_EMAIL=$MAESTRO_PEER_EMAIL`) on a second simulator, accepts the
  DM request (`btn-accept-request-<id>`), and replies. Assert convergence on both.
- **enrollment** — device A (signed in) approves device B's enrollment:
  A → `screen-self-security` → `btn-approve-<requestId>`; B starts from the
  `screen-auth-enrollment` branch (OTP on an already-registered account) and
  reaches the tabs. Recovery path: `btn-enroll-recovery` → `input-recovery-key`.
- **realtime** — two devices in the same channel/DM; peer sends, assert the
  message appears on device A WITHOUT a manual refresh (LiveKit data-only). Needs
  `EXPO_PUBLIC_LIVEKIT_URL` baked into both dev builds.
- **blocking** — block the peer from `screen-user` (`btn-block`), assert their
  messages are filtered, then `btn-unblock`. Needs the peer + an existing convo.
- **push-tap** — background the app, deliver a content-free push, tap it, assert
  deep-link into the conversation. Needs EAS + APNs/FCM creds (#344 operational);
  on a simulator, use `xcrun simctl push` with a crafted payload.
