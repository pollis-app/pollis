import React, { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQueryClient } from "@tanstack/react-query";
import { Volume2 } from "lucide-react";
import {
  usePreferences,
  clampRemoteUserVolume,
  userIdFromVoiceIdentity,
  REMOTE_USER_VOLUME_MIN,
  REMOTE_USER_VOLUME_MAX,
  REMOTE_USER_VOLUME_DEFAULT,
  type PreferencesData,
} from "../../hooks/queries/usePreferences";
import { useAppStore } from "../../stores/appStore";

interface RemoteUserVolumeSliderProps {
  identity: string;
  participantName: string;
}

/**
 * Inline per-remote-user volume slider rendered alongside a participant
 * row in the voice channel. The 0.0..2.0 multiplier is applied by the
 * Rust mixer (see `set_remote_user_volume`) and persisted in the user's
 * preferences blob via `save_preferences`.
 *
 * Designed to live inside `NavigableList`'s `controls` slot — the input
 * is focusable, so ArrowLeft/Right + native range-slider keyboard
 * controls (Left/Right/Home/End) work as expected.
 */
export const RemoteUserVolumeSlider: React.FC<RemoteUserVolumeSliderProps> = ({
  identity,
  participantName,
}) => {
  const { mutation, query } = usePreferences();
  const currentUser = useAppStore((s) => s.currentUser);
  const queryClient = useQueryClient();

  const userId = userIdFromVoiceIdentity(identity);
  const stored = query.data?.user_volumes?.[userId];
  const value = clampRemoteUserVolume(stored ?? REMOTE_USER_VOLUME_DEFAULT);

  const handleChange = useCallback(
    (next: number) => {
      const clamped = clampRemoteUserVolume(next);

      // Push the live value into the mixer immediately so the change is
      // audible while the user drags — no debounce, no waiting on the
      // remote prefs save.
      invoke("set_remote_user_volume", { userId, volume: clamped }).catch(
        (e) => {
          console.warn(
            "[RemoteUserVolumeSlider] set_remote_user_volume failed:",
            e,
          );
        },
      );

      // Persist via the existing preferences blob. Optimistically update
      // the React Query cache so other reads see the new value before the
      // round-trip resolves.
      const prev: PreferencesData = query.data ?? {};
      const prevVolumes = prev.user_volumes ?? {};
      const nextVolumes: { [userId: string]: number } = { ...prevVolumes };
      if (Math.abs(clamped - REMOTE_USER_VOLUME_DEFAULT) < 0.001) {
        delete nextVolumes[userId];
      } else {
        nextVolumes[userId] = clamped;
      }
      const nextPrefs: PreferencesData = {
        ...prev,
        user_volumes:
          Object.keys(nextVolumes).length > 0 ? nextVolumes : undefined,
      };
      if (currentUser) {
        queryClient.setQueryData(
          ["user", "preferences", currentUser.id],
          nextPrefs,
        );
      }
      mutation.mutate(nextPrefs);
    },
    [userId, mutation, query.data, queryClient, currentUser],
  );

  const pct =
    ((value - REMOTE_USER_VOLUME_MIN) /
      (REMOTE_USER_VOLUME_MAX - REMOTE_USER_VOLUME_MIN)) *
    100;

  return (
    <div className="flex items-center gap-2">
      <Volume2 size={12} style={{ color: "var(--c-text-dim)" }} />
      <input
        type="range"
        min={REMOTE_USER_VOLUME_MIN}
        max={REMOTE_USER_VOLUME_MAX}
        step={0.05}
        value={value}
        onChange={(e) => handleChange(Number(e.target.value))}
        aria-label={`Output volume for ${participantName}`}
        data-testid={`voice-volume-slider-${userId}`}
        className="
          w-20 h-1 rounded-md appearance-none cursor-pointer
          focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)]
          [&::-webkit-slider-thumb]:appearance-none
          [&::-webkit-slider-thumb]:w-3
          [&::-webkit-slider-thumb]:h-3
          [&::-webkit-slider-thumb]:rounded-full
          [&::-webkit-slider-thumb]:cursor-pointer
          [&::-moz-range-thumb]:w-3
          [&::-moz-range-thumb]:h-3
          [&::-moz-range-thumb]:rounded-full
          [&::-moz-range-thumb]:border-none
          [&::-moz-range-thumb]:cursor-pointer
        "
        style={{
          background: `linear-gradient(to right, var(--c-accent) 0%, var(--c-accent) ${pct}%, var(--c-border-active) ${pct}%, var(--c-border-active) 100%)`,
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          ["--thumb-color" as any]: "var(--c-accent)",
        }}
      />
    </div>
  );
};
