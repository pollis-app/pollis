import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

/** Mirrors Rust `PermissionState` (`src-tauri/src/commands/media_permissions.rs`); `perSession` = Linux brokered. */
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

/** Live OS permission status; refetches on focus. */
export function useMediaPermissions() {
  return useQuery({
    queryKey: MEDIA_PERMISSIONS_KEY,
    queryFn: () => invoke<MediaPermissions>("get_media_permission_status"),
    refetchOnWindowFocus: true,
    staleTime: 0,
  });
}

/** Open the OS privacy settings (#434); rejects on Linux. */
export async function openPrivacySettings(kind: "camera" | "microphone"): Promise<void> {
  await invoke("open_privacy_settings", { kind });
}

/** Revoke the OS permissions, then refetch (macOS clears; Windows opens settings). */
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
