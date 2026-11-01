# Dependency + image vulnerability scan — 2026-10-13 (#1296)

Triggered by [rust-lang/rust#164250](https://github.com/rust-lang/rust/issues/164250). Covers every
package manager in the repo and every container we run. Re-run the same commands
before closing a future scan.

## rustc #164250 (borrow checker)

The compiler accepts a struct whose `Drop` impl can observe a borrow after the
borrowed value is gone. The trigger is a generic or lifetime-parameterised type
with a bound (`T: 'static`, `'a: 'b`) plus a custom `Drop`. Every stable rustc since
1.4 is affected, and no fixed release exists yet (open, `P-high`). Upgrading the
toolchain therefore changes nothing, so `rust-toolchain.toml` stays on 1.96.0.

- **Our code: not reachable.** Every `impl Drop` in the workspace (27 of them,
  `git grep -nE 'impl(<.*>)?\s+Drop\s+for'`) is on a concrete type with no
  generic or lifetime parameter, so none of them has the shape the bug needs.
- **Dependencies:** exploiting it takes a deliberately malicious `Drop`. A
  crate that malicious could already use `unsafe`, so the defence is the
  existing supply-chain gate (cargo-vet review provenance + cargo-deny sources).
- **Action when a fix ships:** bump `rust-toolchain.toml` and the
  `dtolnay/rust-toolchain@<ver>` refs together (the file's own note). Avoid
  1.98.0: it has a vtable miscompilation that 1.98.1 fixes.

## Rust (`cargo deny check`, `cargo audit`, `cargo vet --locked`)

`cargo deny check` passes all four checks. Every advisory `cargo audit` reports is
already triaged in `deny.toml`, with one exception: three new libcrux advisories
(RUSTSEC-2026-0329/0330/0331). Those crates appear in `Cargo.lock` only as
optional features of openmls that we never enable, so cargo-deny's resolved graph
does not contain them and they are never compiled.

| Change | Why |
|---|---|
| `chacha20` 0.10.1 → 0.10.2 (lockfile) | 0.10.1 was **yanked**: its SSE2 backend used an SSE4.1 intrinsic, so the RNG (`rand` 0.10 → ChaCha) faulted with SIGILL on SSE2-only x86 CPUs. We reviewed the delta and recorded it in `supply-chain/audits.toml`. |

**Update 2026-10-17:** three new `zip` advisories (RUSTSEC-2026-0367/0368/0369)
match `zip` 0.6.6, a build-only dependency of `webrtc-sys-build`. It unpacks
LiveKit's prebuilt libwebrtc archive, which is linked into the app anyway, so
extraction bugs grant nothing extra. No upstream release moves off `zip` 0.6
yet, so they are ignored in `deny.toml` with that reasoning.

`spin` 0.9.8 is also yanked. It comes in only via `lazy_static`'s `spin_no_std`
path, and 0.9.8 is the last 0.9.x release, so there is nothing to move to.

## JavaScript

**Root workspace** (frontend, e2e, pollis-delivery): 90 → 12 advisories.

- The lockfile still carried a stale `mobile` importer. Mobile has its own
  lockfile, so a plain re-resolve dropped it, along with every mobile-only
  finding it produced.
- `pnpm-workspace.yaml` `overrides` raise the rest to a patched floor **inside
  the major the parent asked for**: undici, ws, tar-fs, brace-expansion,
  ip-address, nanoid, browserslist, baseline-browser-mapping, source-map-js,
  postcss-selector-parser 6.x and @babel/core.

The 12 that remain are all dev, build or test tooling, never shipped. Each one's
only fix is a new major (or none exists):

| Package | Reaches us via | Fix |
|---|---|---|
| sharp 0.34 | wrangler → miniflare (local `wrangler dev` only) | 0.35 (next wrangler) |
| esbuild 0.27 | wrangler (dev-server file read) | 0.28 (next wrangler) |
| extract-zip, basic-ftp 5, deepmerge-ts 5, fast-xml-parser 4, http-cache-semantics | e2e → webdriverio 8 | webdriverio 9 / none |
| braces 3.0.3, postcss-selector-parser 6 | tailwindcss 3 (build) | none / tailwind 4 |

The desktop renderer's runtime dependencies have no advisories.

**Mobile** (`mobile/pnpm-lock.yaml`): 4 advisories → 3 build-time.

- **Fixed:** `decode-uri-component` 0.2.2 (GHSA-vcc3-ghjq-m6fr) runs on every
  incoming `pollis://` link, through expo-router → query-string. Its fallback
  decoder is super-linear: a 2.4 KB malformed link froze the JS thread for
  about 3 s on a laptop. The upstream fix (0.5.0) is ESM-only, and query-string 7
  `require`s it. `patches/decode-uri-component@0.2.2.patch` therefore backports
  0.5.0's linear decoder into the CJS file (since 2026-10-31 0.5.1's, see below). A differential test against 0.5.0
  over 50k random inputs showed identical output. `tests/deep-link-decode.test.ts`
  fails if the patch stops applying.
- **Left, build-time only:** `braces` (Metro file map, no fix). `uuid` 7 and
  `node-forge` were fixed on 2026-10-31 (below).

## Update 2026-10-31: Dependabot sweep

36 open alerts. What changed:

| Alert(s) | Fix |
|---|---|
| `zip` < 9.0.5 in `pollis-core` (3 advisories, all reader-side) | `zip` 4 → 9.0.5. pollis-core only writes zips (the mobile export bundle), so none was reachable, but the manifest alerts close. |
| `decode-uri-component` ≤ 0.5.0 (GHSA-qr28-fpjm-wq7f) | 0.5.0's fix kept a replace-map loop that ran one full-string regex per distinct percent run, which is quadratic. The patch now backports 0.5.1. It matches 0.5.1 on 200k random inputs, and the advisory's PoC drops from 45 s to under 0.1 s. `tests/deep-link-decode.test.ts` pins it. |
| `seroval` < 1.6.11 (5 advisories, the desktop router) | override floor `~1.6.12` (and `seroval-plugins`), inside router-core's `^1.6.7` |
| `undici` < 7.30.1, `postcss-selector-parser` < 6.1.5 | raised the existing override floors |
| `esbuild` 0.27.3 | root `wrangler` ^4.76 → ^4.107.1, the version `pollis-delivery` already uses (esbuild 0.28) |
| `sharp` < 0.35.5 (3 advisories) | `wrangler` ^4.107.1 → ^4.151.3 (root + `pollis-delivery`): miniflare 5, sharp 0.35.5. Local `wrangler dev` only. The DS bundle, `deploy --containers-rollout`, `containers push/list/instances` are unchanged. |
| `zip` 0.6.6 (Cargo.lock) | `webrtc-sys-build` 0.3.19 → 0.3.20, whose only change is zip 9. Not 0.3.21: that also swaps the prebuilt libwebrtc and belongs with a LiveKit bump. |
| mobile `node-forge` ≤ 1.4.0 | override floor `^1.4.2`. The advisory lists no fix, but 1.4.2's `rsa.js` requires a canonical DER DigestInfo, which is the fix. Build-time (`@expo/cli`, `@expo/code-signing-certificates`). |
| mobile `uuid` 7 | removed. `pnpm dedupe` moved expo-splash-screen off a stale `@expo/config-plugins` (xcode 3 → uuid 7) onto the 57.0.11 the app already uses. |
| `clipboard-win` 5.4.1 (yanked) | 5.4.3. Windows clipboard, via `tauri-plugin-clipboard-manager` → arboard. |

Still open, each with its reason:

| Package | Why it stays |
|---|---|
| `zip` 4.6.1 (Cargo.lock) | `tauri-plugin-updater` pins it on Windows. Only 2.14.1 moves to zip 9, and that needs tauri ^2.12 (we are on 2.11). The archive is minisign-verified before extraction. The bump changes the auto-updater on every platform. CI cannot build Windows or macOS without cutting a release, so it waits for a release we can test end to end: an old build updating to the new one on all three platforms. |
| `rand` 0.7.3 | build-time `phf_generator`, via tauri-utils → kuchikiki. 0.7 has no fix, and the bug needs a custom logger. |
| `lru` 0.12 | `ratatui` 0.28, in the TUI only. The fix is lru 0.16 (a ratatui upgrade). |
| `glib` 0.18 | GTK3 via Tauri on Linux. The fix is 0.20 (gtk4-rs), which Tauri does not use. |
| `libsql-sqlite3-parser` 0.13 | no fix released. The DS and pollis-schema only parse our own migrations and queries. |
| `basic-ftp`, `extract-zip` ×2, `deepmerge-ts`, `fast-xml-parser` | e2e → webdriverio 8 (the tauri-driver WebDriver scenarios). The fix is webdriverio 10, a two-major bump of test tooling. |
| mobile `zod` 3, `deepmerge` | build-time Expo CLI / config loading, pinned by SDK 57. Neither is in the app bundle (checked against the `expo export` source map). zod's fix is 4.x, outside `@expo/cli`'s `^3.25.76`; deepmerge has no fixed release. |

## Server images (`trivy image --severity HIGH,CRITICAL`)

| Image | Before | After |
|---|---|---|
| LiveKit SFU (internet-facing) | v1.10.0: OpenSSL 3.5.5 (1 critical), go-jose, x/crypto, grpc, otel, Go stdlib: 63 fixable | **v1.13.9**: 4 left (x/net, stdlib; no newer upstream build) |
| nginx ingress | 1.29-alpine (EOL mainline): 57 fixable, OpenSSL critical | **1.30-alpine** (stable): 3 (libexpat, pcre2, tiff; modules we don't load) |
| DS / relay / verifiable-log-serve runtime (`debian:bookworm-slim`) | perl-base criticals, fixed in bookworm-security | `apt-get upgrade` in the runtime stage |

The LiveKit 1.10 → 1.13 jump crosses the 1.12 TURN changes. TURN credentials
now carry a TTL, which the server mints at join, so clients are unaffected.
Relaying to private peer IPs is now off by default; our SFU advertises its
public address, so that is unaffected too. The two things our config relies on
are unchanged in 1.13.9: the hardcoded `turns:<domain>:443` URL that nginx's
SNI split depends on, and the `external_tls` listener.
`nginx -V` on 1.30 still lists `--with-stream_ssl_preread_module`.

## Not covered

Native mobile dependencies (Gradle / CocoaPods: RN, Hermes, WebRTC) have no
lockfile-level advisory feed here. They move with the Expo SDK pins.
