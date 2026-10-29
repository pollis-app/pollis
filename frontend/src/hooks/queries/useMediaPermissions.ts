import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

/**
 * Mirrors the Rust `PermissionState` (`src-tauri/src/commands/media_permissions.rs`,
 * camelCase). `perSession`: brokered per session (Linux); `unsupported`: no
 * queryable permission here.
 */
export type PermissionState =
  | "granted"
  | "denied"
  | "notDetermined"
  | "perSession"
  | "unsupported";

export interface MediaPermissions {
  camera: PermissionState;
  microphone: PermissionState;
  screen: PermissionState;
}

export interface RevokeResult {
  applied: boolean;
  note: string | null;
}

export type MediaKind = "camera" | "microphone" | "screen";

const MEDIA_PERMISSIONS_KEY = ["media-permissions"] as const;

/** Live OS permission status; refetches on focus to catch System Settings changes. */
export function useMediaPermissions() {
  return useQuery({
    queryKey: MEDIA_PERMISSIONS_KEY,
    queryFn: () => invoke<MediaPermissions>("get_media_permission_status"),
    refetchOnWindowFocus: true,
    staleTime: 0,
  });
}

/** Deep-link to the OS privacy settings (#434). Rejects on Linux and unknown kinds. */
export async function openPrivacySettings(kind: "camera" | "microphone"): Promise<void> {
  await invoke("open_privacy_settings", { kind });
}

/**
 * Revoke OS permission(s), then refetch: macOS clears the grant, Linux is a
 * no-op success, Windows opens the privacy settings.
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
