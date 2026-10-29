import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";

// #176: pre-warm DNS / TLS / token mint on intent (hover, route entry), so
// `join_voice_channel` pays only the room join. See `prepare_voice_connection`
// in `src-tauri/src/commands/voice.rs`.

// JS-side de-dupe on top of Rust's, so hover spam doesn't even hit IPC.
let inFlightChannelId: string | null = null;
let lastWarmedAt = 0;
let lastWarmedChannelId: string | null = null;
const COALESCE_WINDOW_MS = 2000;

/** Fire `prepare_voice_connection`; idempotent, best-effort, errors swallowed. */
export function warmVoiceChannel(channelId: string | null | undefined): void {
  if (!channelId) {
    return;
  }
  const { currentUser } = appStore;
  if (!currentUser) {
    return;
  }
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
      // Best-effort: failures don't block joining.
    })
    .finally(() => {
      if (inFlightChannelId === channelId) {
        inFlightChannelId = null;
      }
    });
}
