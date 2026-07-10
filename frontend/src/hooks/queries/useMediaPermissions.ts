import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

/**
 * OS-level media permission state, mirroring the Rust `PermissionState` enum
 * (`src-tauri/src/commands/media_permissions.rs`, serde `camelCase`). Keep the
 * union in exact sync with the Rust variants.
 *
 *   - `granted`       — access is granted right now.
 *   - `denied`        — explicitly denied (or restricted by policy).
 *   - `notDetermined` — never asked; the OS prompts on first use.
 *   - `perSession`    — no standing grant; brokered per session (Linux).
 *   - `unsupported`   — no queryable permission for this device on this OS.
 */
export type PermissionState =
  | "granted"
  | "denied"
  | "notDetermined"
  | "perSession"
  | "unsupported";

/** Mirrors the Rust `MediaPermissions` struct (serde `camelCase`). */
export interface MediaPermissions {
  camera: PermissionState;
  microphone: PermissionState;
  screen: PermissionState;
}

/** Mirrors the Rust `RevokeResult` struct (serde `camelCase`). */
export interface RevokeResult {
  applied: boolean;
  note: string | null;
}

/** The three media kinds, matching the Rust `tccutil`/registry mapping. */
export type MediaKind = "camera" | "microphone" | "screen";

const MEDIA_PERMISSIONS_KEY = ["media-permissions"] as const;

/**
 * Live OS permission status for camera / microphone / screen share. Refetches
 * on window focus so the status reflects changes the user makes in System
 * Settings while Pollis is running.
 */
export function useMediaPermissions() {
  return useQuery({
    queryKey: MEDIA_PERMISSIONS_KEY,
    queryFn: () => invoke<MediaPermissions>("get_media_permission_status"),
    // The whole point is to catch OS-side changes made while we run.
    refetchOnWindowFocus: true,
    staleTime: 0,
  });
}

/**
 * Deep-link to the OS privacy settings for a media kind (issue #434). An app
 * cannot grant/revoke its own OS grant — this just takes the user there. Rejects
 * on Linux (no per-application privacy model) and for unknown kinds.
 */
export async function openPrivacySettings(kind: "camera" | "microphone"): Promise<void> {
  await invoke("open_privacy_settings", { kind });
}

/**
 * Revoke the OS permission(s) for the given kinds, then refetch the live
 * status. On macOS this clears the saved grant (macOS re-prompts next use);
 * on Linux it's a no-op success; on Windows it opens the privacy settings.
 */
export function useRevokeMediaPermissions() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (kinds: MediaKind[]) =>
      invoke<RevokeResult>("revoke_media_permissions", { kinds }),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: MEDIA_PERMISSIONS_KEY });
    },
  });
}
