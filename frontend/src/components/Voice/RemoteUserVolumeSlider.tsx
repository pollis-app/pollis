import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "../../bridge";
import { Volume2 } from "lucide-react";
import {
  usePreferences,
  clampRemoteUserVolume,
  REMOTE_USER_VOLUME_MIN,
  REMOTE_USER_VOLUME_MAX,
  REMOTE_USER_VOLUME_DEFAULT,
  type PreferencesData,
} from "../../hooks/queries/usePreferences";
import { userIdFromVoiceIdentity } from "../../voice/identity";

interface RemoteUserVolumeSliderProps {
  identity: string;
  participantName: string;
}

/**
 * Per-remote-user volume (0..2). Every input event reaches the Rust mixer live; the preferences
 * blob is written once, when the drag or key press ends, so a drag is not N preference saves.
 */
export const RemoteUserVolumeSlider: React.FC<RemoteUserVolumeSliderProps> = ({
  identity,
  participantName,
}) => {
  const { t } = useTranslation("voice");
  const { save, query } = usePreferences();

  const userId = userIdFromVoiceIdentity(identity);
  const stored = query.data?.user_volumes?.[userId];
  const persisted = clampRemoteUserVolume(stored ?? REMOTE_USER_VOLUME_DEFAULT);

  // The thumb follows the pointer; `persisted` only re-seeds it while nothing is being dragged.
  const [value, setValue] = useState(persisted);
  const dirty = useRef(false);
  useEffect(() => {
    if (!dirty.current) {
      setValue(persisted);
    }
  }, [persisted]);

  const handleInput = useCallback(
    (next: number) => {
      const clamped = clampRemoteUserVolume(next);
      dirty.current = true;
      setValue(clamped);
      invoke("set_remote_user_volume", { userId, volume: clamped }).catch((e) => {
        console.warn("[RemoteUserVolumeSlider] set_remote_user_volume failed:", e);
      });
    },
    [userId],
  );

  const commit = useCallback(() => {
    if (!dirty.current) {
      return;
    }
    dirty.current = false;
    const prev: PreferencesData = query.data ?? {};
    const nextVolumes: { [userId: string]: number } = { ...(prev.user_volumes ?? {}) };
    if (Math.abs(value - REMOTE_USER_VOLUME_DEFAULT) < 0.001) {
      delete nextVolumes[userId];
    } else {
      nextVolumes[userId] = value;
    }
    save({
      ...prev,
      user_volumes: Object.keys(nextVolumes).length > 0 ? nextVolumes : undefined,
    });
  }, [userId, save, query.data, value]);

  const pct =
    ((value - REMOTE_USER_VOLUME_MIN) /
      (REMOTE_USER_VOLUME_MAX - REMOTE_USER_VOLUME_MIN)) *
    100;

  return (
    <div className="flex items-center gap-2">
      <Volume2 size={12} className="text-dim" />
      <input
        type="range"
        min={REMOTE_USER_VOLUME_MIN}
        max={REMOTE_USER_VOLUME_MAX}
        step={0.05}
        value={value}
        onChange={(e) => handleInput(Number(e.target.value))}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
        aria-label={t("tile.volumeFor", { name: participantName })}
        data-testid={`voice-volume-slider-${userId}`}
        className="
          w-20 h-1 rounded-md appearance-none cursor-pointer
          focus:outline-none focus:ring-2 focus:ring-accent
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
