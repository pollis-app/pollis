# Client minimum version (#1611)

Pollis can retire old client builds. Every client — desktop, mobile (iOS, Play, sideload) and the
CLI — checks a published version policy at launch, before unlock, sign-in or any DS traffic. A build
below its platform's minimum shows one page: what happened, and the one thing to do. The DS enforces
the same policy with **426 Upgrade Required**, which reaches a build that skipped the launch check
(a long-running session).

Publishing it (and the rollout rules) is `docs/deployments.md` → "Client minimum version".

## The manifest

`https://cdn.pollis.com/client-policy.json` for builds pointed at the prod DS,
`https://cdn.pollis.com/dev/client-policy.json` for any other DS. Static, no auth, no dependency on
the DS. Written only by `scripts/client-policy.mjs`.

```json
{ "v": 1,
  "minimum": { "ios": "1.0.21", "android-play": "1.0.21", "android-sideload": "1.0.21",
               "macos": "1.17.0", "windows": "1.17.0", "linux": "1.17.0", "cli": "1.17.0" },
  "message": null }
```

**The rule** — `pollis-api/src/client_policy.rs`, `ClientPolicy::evaluate`, the one implementation
used by `pollis-core` (every client), the DS and the publisher:

- blocked iff `version < minimum[platform]`, semver precedence (`1.17.0-rc.1 < 1.17.0`, build
  metadata ignored, numeric components compared as numbers);
- exactly at the minimum is allowed;
- a missing key means no minimum;
- an unparseable version on either side is allowed (fail open).

Two parsers: `ClientPolicy::parse` (clients and the DS) tolerates unknown platform keys and fields,
so a platform added later does not break an older build; `parse_strict` (the publisher) refuses
them, so a typo cannot silently set no minimum. Both reject a format other than `v: 1`, a minimum
that is not `MAJOR.MINOR.PATCH`, and a message over 500 characters.

## Which build is this

| Product | Platform key | Version | Where it is set |
|---|---|---|---|
| Desktop | `macos` / `windows` / `linux` (compile target) | `CARGO_PKG_VERSION` — the release tag, stamped by `scripts/bump-version.sh` (dev builds: the `1.1.0` placeholder) | `ClientIdentity::desktop()` in `src-tauri/src/lib.rs` setup |
| CLI | `cli` | same | `ClientIdentity::cli()` in `pollis-tui/src/main.rs` |
| iOS | `ios` | `nativeApplicationVersion` (the store binary, never the OTA bundle) | `init_pollis` `client_platform` / `client_version`, from `mobile/lib/buildInfo.ts` |
| Android | `android-play` (install channel `store`) or `android-sideload` (`sideload`, `dev`) | same | same |

Play vs sideload comes from the install channel baked into the binary (#1443), not the runtime
installer package: the two are differently signed binaries that cannot update each other, so the
baked channel is exact, and it needs no extra native module. Builds from before this feature send
nothing and are never checked.

The identity lives in `Config::client`; `ds_client.rs` (`ds_headers`) sends it on every DS request
as `X-Pollis-Client-Platform` / `X-Pollis-Client-Version`. The notification extension's headless
fetch sends **no** version headers: its saved config is from the app's last launch, and a store
auto-update can replace the binary without the app being opened, so a stale version would earn a
rich push a 426.

## The client check (`pollis-core/src/client_policy.rs`)

`check(config, data_dir, plan, timeout)`:

1. Fetch the policy URL once, bounded by `FETCH_TIMEOUT` (4 s), never retried in a loop. The
   overlay is respected the way the updater does (`FetchPlan`): a strict overlay with no circuit
   does not fetch at all.
2. A good fetch is decisive in either direction and replaces the cache. A **404** is a good
   fetch of "nothing published": no minimums, exactly as the DS reads it.
3. A failed, timed-out, non-2xx (other than 404) or malformed fetch falls back to the cache.
4. A format this build can't read (a future `v`) is allowed and **clears** the cache, so a block
   recorded under the old format can't stick to a build that can no longer read the new one.
5. No fetch and no cache: allowed.

The cache is `client-policy.json` in the data dir (`dirs_path()`): public data, plain JSON, readable
before unlock — deliberately **not** the SQLCipher DB, which is closed until the PIN. It is
re-validated on read, so a damaged cache blocks nothing. It records its **source** (the URL it was
fetched from) and answers only for that same source; a file without one is ignored. Override
results (the mobile dev file override, and any `inline:` policy) are never cached, so a test
override's verdict cannot outlive it. Nothing else is touched: a blocked build
keeps every byte of local data.

`check_state(state, force)` wraps it for a running app: the verdict is recorded on
`AppState::client_policy`, and a non-forced check within `RECHECK_MIN_INTERVAL` (5 min) of the last
fetch returns the last verdict without a fetch. Launch and the Retry button force; window focus
(desktop) and app resume (mobile) do not.

Once the verdict is blocked, every DS sender in `ds_client.rs` fails locally with
`Error::UpgradeRequired` (`"upgrade_required"`), and a DS `426` does the same after recording the
DS's policy (`record_upgrade_required`) — which also writes the cache (under the published source), so the block holds offline
and across launches. Only the DS's typed body (`error: "upgrade_required"`) latches a block; any
other 426 (a proxy, a misconfigured edge) is an ordinary error. Renderers match the exact text, like `device_revoked`.

Commands (`commands/client_policy.rs`, desktop shims in `src-tauri/src/commands/client_policy.rs`,
mobile arms in `bridge.rs`): `check_client_policy({ force })`, `get_client_policy_status()`, `peek_client_policy()` (the
cache's verdict, no network), and,
mobile-only and dev-only, `set_client_policy_override({ url, policy })`. All return / use
`ClientPolicyStatus { blocked, platform, version, minimum, message, source }` where `source` is
`network | cache | ds | none`.

### Launch never waits for an answer it already has

Launch reads `peek_client_policy` first. If the cache already allows this build, the app goes on
at once and the forced check runs behind it; a block it finds still lands (desktop/mobile flip to
the page, the CLI leaves its UI). If the cache blocks, or there is no cache, launch waits for the
forced check (bounded by `FETCH_TIMEOUT`).

### Dev builds

Unstamped desktop and CLI builds report the workspace placeholder version (`1.1.0`, replaced only
by CI's `scripts/bump-version.sh`). They point at a non-prod DS, so they read the **dev** policy
file, never prod's. A dev minimum above `1.1.0` for `macos` / `windows` / `linux` / `cli` therefore
blocks every dev build; `scripts/client-policy.mjs --env dev` warns when you set one. Mobile dev
builds carry the real `mobile/package.json` version.

### Test overrides

- `POLLIS_CLIENT_POLICY_URL_OVERRIDE` (runtime env, desktop and CLI; a different name from the
  DS's `POLLIS_CLIENT_POLICY_URL`): fetch this instead; `inline:<json>` uses the JSON as if
  fetched. Like the file override it is honoured only by a debug build or one pointed at a
  non-prod DS (`overrides_allowed`), so a release build on prod can't be pointed elsewhere.
- `set_client_policy_override` (mobile dev builds; refused when the DS is prod): persisted in
  `client-policy-override.json`, driven by the `pollis://dev/client-policy?policy=…` / `?clear=1`
  link the Maestro flow opens.
- Playwright: the Tauri mock answers `check_client_policy` from the `clientPolicy` preload or
  `window.__tauriSetClientPolicy`, and `@tauri-apps/plugin-updater` / `-process` / `-shell` are
  mocked (`__POLLIS_UPDATER__ = "none" | "succeed" | "fail"`, `__pollisRelaunched`, `__pollisOpened`).

## What each product shows

- **Desktop** (`frontend/src/components/ClientBlockedScreen.tsx`): the whole window (title bar
  only, no shell, no modal). `App.tsx` runs the check first in `checkStoredSession`, before the
  updater, the session and the PIN. **Update now** runs the Tauri updater (read-only locations get
  their existing explanation first); a failed update, or an updater with nothing newer, swaps it for
  **Download the latest version** (`https://pollis.com/#download`). A package-manager install
  (`detect_managed_install`, the AUR) gets its update command instead of Update now. **Check again**
  forces a re-check and, once allowed, resumes the launch where it stopped. The window `focus`
  event re-checks (throttled by the core); `onUpgradeRequired` in `bridge/invoke.ts` turns any
  command's `upgrade_required` into the page.
- **Mobile**: the same page in place of the whole navigation stack, before the PIN/email screen;
  its button opens the install's update destination (`lib/installChannel.ts`: the App Store once
  `IOS_APP_STORE_ID` is set, TestFlight until then; Play for a Play build; pollis.com/android for
  the sideload APK). Re-checked on `AppState` → `active`; `upgrade_required` from any bridge call
  shows it.
- **CLI**: `--help` / `--version` never touch it. A non-interactive run pays for no fetch before
  its usage error, but a cached block still reports itself and exits 3. On a terminal, the check
  runs before anything opens (cache-first, as above): a blocked build prints what happened, the
  operator message if any, and the install command, and exits **3** (`cli::EXIT_UPGRADE_REQUIRED`;
  1 is a failure, 2 a bad argument). A block found in the background or by a 426 mid-session
  leaves the UI the same way.

Copy is `nav:clientBlocked.*` in all seven locales. The manifest's `message`, when set, replaces the
body text (one language — use it only for something specific).

## The DS (`pollis-delivery/src/client_policy.rs`)

The DS mirrors the same published file (`POLLIS_CLIENT_POLICY_URL`, per environment in
`wrangler.{dev,prod}.jsonc`, forwarded by `worker/index.ts`) rather than holding its own copy: one
source of truth, changed without a deploy. It refreshes lazily — a request that finds the copy older
than a minute spawns one background re-fetch (single-flight; never a timer) — keeps the last good
copy through failures, and treats a 404 as "no minimums". Until the first fetch succeeds nothing is
enforced. Fetches go through `util::http_get` (`Upstream::ClientPolicy`, 5 s).

`enforce` (a router layer inside the rate limiter): a **POST** whose headers declare a known
platform and a version below its minimum gets `426` with `UpgradeRequiredBody { error:
"upgrade_required", platform, version, minimum, message, policy }`. Everything else passes —
requests without both headers (every older build, operator tools), unknown platforms, unparseable
versions, and every GET (`/health`, `/version`, `/v1/ops/*`, `/v1/config`, `/v1/retention/metrics`,
the mirror). The verifier never talks to the DS. `GET /v1/client-policy` returns the policy being
enforced (404 when unconfigured, 503 before the first fetch) — use it to confirm a publish reached
the DS.

**Builds older than this feature cannot show the page**: they send no headers, so the DS lets them
through, and if one ever met a 426 it would surface as an ordinary error. Hence the rollout rule:
ship the clients with no minimums first.

## Tests

| State | Where |
|---|---|
| rule: up to date, at the minimum, below it, missing key, pre-release, unparseable | `pollis-api` unit tests |
| unreachable / timeout / malformed with no cache → allowed; cached block offline → blocked; cache blocked + fresh allowed → allowed; fresh block beats a permissive cache; 404 → no minimums, lifts a cached block; future format → allowed, cache cleared; cache ignored for another source; overrides and `inline:` never cached; override gate; `peek` | `pollis-core` `client_policy::tests` (real HTTP) |
| headers on every DS request; typed 426 → blocked, cached, later calls fail locally; untyped 426 → ordinary error, not latched | `pollis-core` `ds_client::client_policy_tests` |
| 426 body, passes (headers absent, unknown platform, GETs), mirror refresh / 404 / failures | `pollis-delivery/tests/client_policy.rs` |
| desktop: blocked at launch, message, Update now succeeds, update fails / nothing found → download link, AUR command, Check again, focus re-check, 426 mid-session, a stray `upgrade_required` with the core allowed never blocks, an allowing cache doesn't wait, no cache / a cached block waits | `e2e/client-policy.spec.ts` |
| mobile: blocked at cold launch (before PIN), Retry / unblock, store vs sideload destination | `mobile/.maestro/flows/client-policy.yaml`, `mobile/tests/client-policy.test.ts` |
| CLI: interactive (pty) blocked, cached block offline, cache source mismatch, an allowing cache starts at once and a background block still exits 3, non-interactive pays no fetch, `--help` / `--version` never fetch | `pollis-tui/tests/client_policy.rs` |
| publisher: validate, diff, guard above latest release (iOS/Play bounded by the APK), unreadable manifest and invalid published file fail closed, dev placeholder warning, confirm, upload | `scripts/test-client-policy.sh` |

---
_Back to [index.md](./index.md)_
