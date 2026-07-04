# Media Permissions

OS-level camera / microphone / screen-share access controls, surfaced on the
**Security** page (next to Devices — it is an access-control concern, not a
cosmetic preference): a live status line per device, a "revoke on quit"
preference, and a manual "Revoke now" button. Issue #443.

The design bar is honesty: an OS grant means different things on each platform,
so Pollis reports what is actually true rather than faking a uniform model.

## Where it lives

Unlike most command modules this is **not** a thin shim over `pollis-core` —
querying and clearing OS privacy grants is inherently a shell-runtime concern
(macOS TCC, the Windows ConsentStore registry, `ms-settings:` deep-links, the app
bundle identifier). So it lives entirely in `src-tauri`, the same rationale as
`install_kind.rs` / `tray.rs`. The module is `#[cfg(feature = "native-shell")]`
— the headless test harness never exercises it.

- Backend: `src-tauri/src/commands/media_permissions.rs`
- Exit hook: `src-tauri/src/lib.rs` (`RunEvent::ExitRequested`)
- Frontend hook: `frontend/src/hooks/queries/useMediaPermissions.ts`
- UI: the "Media permissions" section of `frontend/src/pages/SecurityPage.tsx`
  (reachable via Cmd+K — the Security entry's keywords include camera/microphone/
  screen/permission/revoke/privacy so a media-minded search still lands here)
- Preference persistence: `revoke_media_on_exit` in the prefs blob
  (`frontend/src/hooks/queries/usePreferences.ts`); pushed to the host on load
  by `useApplyPreferences` and on toggle by the Security page handler

## Commands

| Command | Shape | Purpose |
|---|---|---|
| `get_media_permission_status` | `() -> MediaPermissions { camera, microphone, screen: PermissionState }` | Live status, queried at call time. The renderer refetches on window focus so it reflects changes the user makes in System Settings while Pollis runs. |
| `revoke_media_permissions` | `(kinds: Vec<String>) -> RevokeResult { applied, note }` | Tears down any active capture first, then revokes per platform. `applied` is true only when Pollis actually changed OS state. |
| `set_revoke_media_on_exit` | `(enabled: bool)` | Pushes the "revoke on quit" pref into a host-side `AtomicBool` (managed `MediaPermissionsState`) so the exit hook can read it synchronously. Mirrors `tray_set_close_to_tray`. |

`PermissionState` (serde `camelCase`, mirrored by the TS union): `granted` |
`denied` | `notDetermined` | `perSession` | `unsupported`.

## Per-OS behavior

| | Status source | "Revoke now" |
|---|---|---|
| **macOS** | `AVCaptureDevice::authorizationStatusForMediaType` (camera + mic), `CGPreflightScreenCaptureAccess()` (screen) | `tccutil reset <Service> <bundle-id>` per kind — **clears** the saved grant so macOS re-prompts on next use (not a permanent deny) |
| **Linux** | `perSession` for all three — no TCC-equivalent standing grant; access is brokered per session (PipeWire portal / device nodes) | No-op success with an explanatory note — there is no standing grant to clear |
| **Windows** | camera/mic from the privacy ConsentStore registry (`…\CapabilityAccessManager\ConsentStore\{webcam,microphone}\NonPackaged`, `Value` = `Allow`/`Deny`); screen → `unsupported` | Opens the `ms-settings:` privacy deep-link (no per-app revoke API for desktop apps) |

The macOS TCC service names are `Camera`, `Microphone`, `ScreenCapture`; the
bundle id comes from `AppHandle::config().identifier` (`com.pollis.app`).

## Revoke-on-quit

The preference is a device-shaped flag persisted in the prefs blob and pushed to
the host on change (via `useApplyPreferences` and the toggle handler). At
shutdown the `ExitRequested` hook — which already stops screen-share, camera, and
voice — reads the `MediaPermissionsState` atomic **synchronously** (no async prefs
fetch at exit) and, if enabled, best-effort runs the macOS `tccutil reset` for all
three kinds. No-op on Linux/Windows. This covers Cmd-Q, tray Quit (`app.exit(0)`),
and non-tray window close uniformly.

## UI notes

Per the repo rules the "Revoke now" confirmation is **not** a modal — clicking it
swaps the button in place for an inline Confirm / Cancel row. Status pills use
solid token colors (no glow). Copy is tailored per-OS so the user is never told
"revoked" when the platform can only re-prompt (macOS) or defer to system
settings (Windows).
