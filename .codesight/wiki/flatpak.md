# Flatpak distribution — DEFERRED

> **Status: deferred.** No Flatpak is currently built, shipped, or supported. The files under `flatpak/` are a parked starting point so a future engineer can pick this up without doing the discovery from scratch. Tracked in [issue #230](https://github.com/actuallydan/pollis/issues/230).
>
> Nothing in the production build, release pipeline, or runtime currently references any of this. The repo is unchanged from a release-engineering standpoint.

## Why deferred

Flatpak adds genuine value (Flathub discoverability, sandboxed install, GNOME Software / KDE Discover / Steam Deck reach), but:

- Flathub review is 1–4 weeks of synchronous back-and-forth on a first submission.
- The sandbox interactions with our voice stack (cpal → PulseAudio) and keyring (libsecret) need real on-device verification.
- The deployment pipeline and in-app updater both need Flatpak-aware branches before the listing can go live, which is non-trivial to land safely.

We'd rather not carry partially-working scaffolding in `desktop-release.yml` while this sits. When someone has the bandwidth to drive it through review, the prep work below should save a day or two of plumbing.

## What's done

These files exist in the repo and are inert — they are not referenced by any build step, workflow, or runtime code.

- `flatpak/com.pollis.app.yaml` — Flathub manifest skeleton:
  - Targets `org.gnome.Platform//46` (still ships WebKitGTK 4.1 natively, which is what Tauri 2 uses on Linux).
  - Sandbox `finish-args` covering: network, Wayland + X11 fallback, GPU (`--device=dri`), PulseAudio for voice, `org.freedesktop.secrets` for the keyring crate, `org.freedesktop.Notifications` for desktop alerts. No broad `--filesystem=` grant — file pickers go through xdg-desktop-portal.
  - Build module assumes the prebuilt `.deb` we already produce: extract, copy `usr/bin/Pollis` → `/app/bin/pollis`, patch `Pollis.desktop` to the app-id, rename icons.
  - Default `url:` points at the `cdn.pollis.com` release URL with a placeholder `sha256` (`0000…`). Local `flatpak-builder` runs need this updated to a real release.
- `flatpak/com.pollis.app.metainfo.xml` — AppStream metadata required by Flathub. Includes summary, description, categories, links, OARS rating, a single placeholder `<release>` entry.

## What's NOT done (pick up here)

The roadmap below is in the order someone resuming this work should tackle it. Each item is a discrete piece of work; none is started.

### 1. In-app updater hard-stop for Flatpak

`src-tauri/src/commands/install_kind.rs` already has the `ManagedInstallKind` enum used by the AUR hard-stop screen. Add a `Flatpak` variant that:

- Detects via `std::env::var_os("FLATPAK_ID").is_some()` OR `std::path::Path::new("/.flatpak-info").exists()` (the env var is the primary signal; the file is a backup against stripped envs).
- `display_name()` returns `"Flatpak"`.
- `update_command()` returns `"flatpak update com.pollis.app"`.

Then widen `frontend/src/components/ManagedInstallScreen.tsx`'s `kind` union from `"aur"` to `"aur" | "flatpak"`. The screen itself is data-driven and needs no JSX changes.

This is mandatory before any Flatpak ships — Flathub explicitly forbids self-updating apps, and the sandbox makes it impossible regardless (read-only OSTree branch).

### 2. Local-build smoke test path

There's no way to build the Flatpak locally today. Decide whether that's a `make` target, a script under `scripts/`, or just instructions. The build needs:

- `flatpak` + `flatpak-builder` installed on the host.
- `flatpak install --user flathub org.gnome.Platform//46 org.gnome.Sdk//46`.
- A real `.deb` to consume — either a recently released one (update the manifest's `url:` + `sha256:`) or a locally-built one rewritten as `path: ../src-tauri/target/.../pollis_*.deb`.
- `flatpak-builder --user --install-deps-from=flathub --force-clean --repo=flatpak/repo flatpak/build-dir flatpak/com.pollis.app.yaml`.
- `flatpak build-bundle flatpak/repo pollis.flatpak com.pollis.app` → single-file install.
- Verify on a real Linux box: voice capture+playback works (PulseAudio socket), identity persists across launches (libsecret), notifications fire.

### 3. CI job (optional, only if we want per-tag verification)

A `build-flatpak` job in `.github/workflows/desktop-release.yml` that depends on `build-linux`, downloads the `.deb` artifact, rewrites the manifest's archive source from `url:` to `path: pollis.deb`, runs `flatpak-builder`, and uploads a `.flatpak` artifact. We had a working draft of this; it was reverted to keep the deferral clean. Re-derive from this article when ready.

This is *not* the distribution channel — Flathub's own buildbot does that once we're listed. The CI job exists only as a smoke test that the manifest still works against the latest binary.

### 4. Flathub submission

PR to [`flathub/flathub`](https://github.com/flathub/flathub) with the manifest. Things reviewers usually flag:

- Prebuilt-binary policy. Flathub prefers source builds; for proprietary apps they grant exceptions, but expect to negotiate. If denied, we'd need `flatpak-cargo-generator` + `flatpak-node-generator` to vendor the entire cargo + npm dependency graph as Flatpak sources — a sizeable chunk of work.
- Metainfo completeness — at minimum we'd want screenshots, a real release history with dates, and a maintainer contact.
- Permissions justification — the secrets-service talk-name is the one most likely to draw a question; "encrypted messenger uses libsecret for the device identity key" is the answer.
- Update cadence expectations — Flathub's buildbot can rebuild from a git tag automatically once configured.

Expect 1–4 weeks of back-and-forth on first submission.

### 5. Decisions still open

- **Versioning sync.** Flathub releases follow our git tags; the metainfo `<release>` history needs to be kept current. We'd want this in `desktop-release.yml` (or a release script) so it doesn't drift.
- **Screenshots.** Required for Flathub. We don't have canonical screenshots committed anywhere — would need 3–5 PNGs hosted at a stable URL.
- **Beta channel.** Flathub supports `--beta` branches if we want a separate prerelease track. Probably not for v1.

## Sandbox capability reference (for when this resumes)

Quick lookup of what each `finish-arg` in the manifest is for:

| Need | finish-arg |
|---|---|
| Turso, R2, LiveKit, OTP delivery | `--share=network` |
| Wayland / X11 / GPU | `--socket=wayland`, `--socket=fallback-x11`, `--device=dri` |
| Voice capture + playback (cpal → PulseAudio / pipewire-pulse) | `--socket=pulseaudio` |
| Identity key + session token (keyring crate → libsecret) | `--talk-name=org.freedesktop.secrets` |
| Desktop notifications | `--talk-name=org.freedesktop.Notifications` |
| File pickers, drag-drop | xdg-desktop-portal — automatic, no broad `--filesystem=` grant |

## Common pitfalls (from research, not yet hit)

- **Voice fails inside the sandbox** — usually a missing `--socket=pulseaudio` or a host running pipewire without the pulse shim. Pure JACK / non-pulse pipewire is out of scope.
- **Keyring crate returns "no backend"** — `--talk-name=org.freedesktop.secrets` must be present, and the host must run `gnome-keyring-daemon` or `kwallet`. KDE Plasma's kwallet has a secret-service compatibility shim and works.
- **WebKit doesn't paint** — the GNOME Platform version must include WebKitGTK 4.1. Pinned to `46`. Bumping to `47+` requires migrating Tauri to webkitgtk-6.0 first.
