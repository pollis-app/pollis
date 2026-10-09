// The audio page of the media viewer (#1248): desktop's audio lightbox
// (`AudioPlayer`, autoplaying) on expo-audio. Play/pause, ten-second seeks
// and a tappable progress track that is also a screen-reader "adjustable".
// The file is the named local copy of the decrypted bytes
// (`useNamedMediaUri`), because AVPlayer picks a decoder by extension.

import { useEffect, useRef, useState } from "react";
import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { setAudioModeAsync, useAudioPlayer, useAudioPlayerStatus } from "expo-audio";
import { useNamedMediaUri } from "../../hooks/useNamedMediaUri";
import { IconButton } from "../ui";
import { Icon } from "../icons";
import { MediaStatus } from "./MediaStatus";
import { formatClock } from "../../lib/media/viewer";
import { formatBytes } from "../../lib/exportArchive";
import { fonts, layout, semantic, space, type as ty } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

const SEEK_STEP_S = 10;

export function AudioPage({
  attachment,
  width,
  height,
  active,
  testID,
}: {
  attachment: MessageAttachment;
  width: number;
  height: number;
  active: boolean;
  testID?: string;
}) {
  const { t } = useTranslation("mobile");
  const { uri, error } = useNamedMediaUri(attachment, active);
  const player = useAudioPlayer(uri ? { uri } : null);
  const status = useAudioPlayerStatus(player);
  const [trackWidth, setTrackWidth] = useState(0);
  const autoplayed = useRef(false);

  // Play through the ringer switch, as every messenger's voice/audio
  // playback does.
  useEffect(() => {
    void setAudioModeAsync({ playsInSilentMode: true }).catch(() => {
      // Best effort: playback still works, just respects the silent switch.
    });
  }, []);

  // Desktop's audio lightbox autoplays; so does this, once, when loaded.
  useEffect(() => {
    if (active && status.isLoaded && !autoplayed.current) {
      autoplayed.current = true;
      player.play();
    }
  }, [active, status.isLoaded, player]);

  useEffect(() => {
    if (!active) {
      player.pause();
    }
  }, [active, player]);

  if (error) {
    return (
      <View testID={testID} style={{ width, height }}>
        <MediaStatus state="error" />
      </View>
    );
  }
  if (!uri || !status.isLoaded) {
    return (
      <View testID={testID} style={{ width, height }}>
        <MediaStatus state="loading" />
      </View>
    );
  }

  const duration = status.duration > 0 ? status.duration : 0;
  const current = Math.min(status.currentTime, duration || status.currentTime);
  const progress = duration > 0 ? current / duration : 0;
  const clock = t("media.audioPosition", {
    current: formatClock(current),
    total: formatClock(duration),
  });

  const seekTo = (seconds: number) => {
    const bounded = Math.max(0, duration > 0 ? Math.min(duration, seconds) : seconds);
    void player.seekTo(bounded);
  };

  const togglePlay = () => {
    if (status.playing) {
      player.pause();
      return;
    }
    // A finished track starts over rather than doing nothing.
    if (duration > 0 && current >= duration - 0.25) {
      void player.seekTo(0);
    }
    player.play();
  };

  return (
    <View
      testID={testID}
      style={{ width, height, alignItems: "center", justifyContent: "center", padding: space.xxl }}
    >
      <View
        style={{
          width: "100%",
          maxWidth: 420,
          gap: space.xl,
        }}
      >
        <View style={{ flexDirection: "row", alignItems: "center", gap: space.md }}>
          <Icon.headphones size={22} color={semantic.dim} />
          <View style={{ flex: 1, minWidth: 0 }}>
            <Text numberOfLines={2} style={[ty.body, { fontFamily: fonts.medium, color: semantic.text }]}>
              {attachment.filename}
            </Text>
            {attachment.file_size > 0 ? (
              <Text style={ty.meta}>{formatBytes(attachment.file_size)}</Text>
            ) : null}
          </View>
        </View>

        {/* Tap anywhere on the track to seek there. */}
        <Pressable
          testID="slider-media-audio"
          accessibilityRole="adjustable"
          accessibilityLabel={t("media.audioProgress")}
          accessibilityValue={{ text: clock }}
          accessibilityActions={[{ name: "increment" }, { name: "decrement" }]}
          onAccessibilityAction={(e) => {
            if (e.nativeEvent.actionName === "increment") {
              seekTo(current + SEEK_STEP_S);
            } else if (e.nativeEvent.actionName === "decrement") {
              seekTo(current - SEEK_STEP_S);
            }
          }}
          onLayout={(e) => setTrackWidth(e.nativeEvent.layout.width)}
          onPress={(e) => {
            if (trackWidth > 0 && duration > 0) {
              seekTo((e.nativeEvent.locationX / trackWidth) * duration);
            }
          }}
          style={{ minHeight: layout.touchMin, justifyContent: "center" }}
        >
          <View style={{ height: 6, borderRadius: 3, backgroundColor: semantic.high, overflow: "hidden" }}>
            <View
              style={{
                width: `${Math.round(progress * 1000) / 10}%`,
                height: 6,
                backgroundColor: semantic.accent,
              }}
            />
          </View>
        </Pressable>
        <Text testID="text-media-audio-clock" style={[ty.meta, { textAlign: "center" }]}>
          {clock}
        </Text>

        <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "center", gap: space.xxl }}>
          <IconButton
            testID="btn-media-audio-back"
            filled
            accessibilityLabel={t("media.seekBack")}
            onPress={() => seekTo(current - SEEK_STEP_S)}
            icon={<Icon.seekBack size={20} />}
          />
          <Pressable
            testID="btn-media-audio-play"
            accessibilityRole="button"
            accessibilityLabel={status.playing ? t("media.pause") : t("media.play")}
            onPress={togglePlay}
            style={({ pressed }) => ({
              width: 56,
              height: 56,
              borderRadius: 28,
              alignItems: "center",
              justifyContent: "center",
              backgroundColor: semantic.accent,
              opacity: pressed ? 0.85 : 1,
            })}
          >
            {status.playing ? (
              <Icon.pause size={24} color={semantic.onAccent} />
            ) : (
              <Icon.play size={24} color={semantic.onAccent} />
            )}
          </Pressable>
          <IconButton
            testID="btn-media-audio-forward"
            filled
            accessibilityLabel={t("media.seekForward")}
            onPress={() => seekTo(current + SEEK_STEP_S)}
            icon={<Icon.seekForward size={20} />}
          />
        </View>
      </View>
    </View>
  );
}
