import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../stores/appStore";

// Issue #176: pre-warm DNS / TLS / token mint to LiveKit on user "intent"
// (hover, route entry) so the actual `join_voice_channel` only pays the
// WebSocket-upgrade + room-join round trip, not also the cold connection
// setup. Mirrors livekit-client's `room.prepareConnection(url, token)`,
// which has no Rust-crate equivalent — see `prepare_voice_connection` in
// `src-tauri/src/commands/voice.rs` for the implementation.

// In-flight de-dupe at the JS layer too: spamming hover should not even
// hit the IPC bridge for the same channel within a short window. The
// Rust side has its own de-dupe + TTL; this is just a small extra guard.
let inFlightChannelId: string | null = null;
let lastWarmedAt = 0;
let lastWarmedChannelId: string | null = null;
const COALESCE_WINDOW_MS = 2000;

/**
 * Fire `prepare_voice_connection` for `channelId`. Safe to call eagerly on
 * hover/keyboard navigation — idempotent and best-effort. Errors are swallowed
 * since this is a UX optimisation, not a correctness boundary.
 */
export function warmVoiceChannel(channelId: string | null | undefined): void {
  if (!channelId) {
    return;
  }
  const { currentUser } = useAppStore.getState();
  if (!currentUser) {
    return;
  }
  // Coalesce: same channel within COALESCE_WINDOW_MS is a no-op.
  const now = Date.now();
  if (
    lastWarmedChannelId === channelId &&
    now - lastWarmedAt < COALESCE_WINDOW_MS
  ) {
    return;
  }
  if (inFlightChannelId === channelId) {
    return;
  }
  inFlightChannelId = channelId;
  invoke("prepare_voice_connection", {
    channelId,
    userId: currentUser.id,
    displayName: currentUser.username ?? currentUser.id,
  })
    .then(() => {
      lastWarmedChannelId = channelId;
      lastWarmedAt = Date.now();
    })
    .catch(() => {
      // Best-effort. Token mint or HTTPS probe failures don't block joining.
    })
    .finally(() => {
      if (inFlightChannelId === channelId) {
        inFlightChannelId = null;
      }
    });
}
