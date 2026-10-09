// One video page of the media viewer (#1248): expo-video with the platform's
// own playback controls, as desktop's lightbox uses the `<video controls>`
// element. The file is the named local copy of the decrypted bytes
// (`useNamedMediaUri`) — AVPlayer picks a decoder by extension — and is
// taken only while the page is on screen, so a roll of videos never holds
// more than the visible one in plaintext.

import { useEffect } from "react";
import { View } from "react-native";
import { useVideoPlayer, VideoView } from "expo-video";
import { useNamedMediaUri } from "../../hooks/useNamedMediaUri";
import { MediaStatus } from "./MediaStatus";
import type { MessageAttachment } from "../../types";

export function VideoPage({
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
  const { uri, error } = useNamedMediaUri(attachment, active);
  // `useCaching` stays off (the default): the player must not keep its own
  // copy of decrypted video.
  const player = useVideoPlayer(uri ? { uri } : null, (p) => {
    p.loop = false;
  });

  // Swiping away pauses: a hidden page must not keep playing.
  useEffect(() => {
    if (!active) {
      player.pause();
    }
  }, [active, player]);

  if (!active || (!uri && !error)) {
    return (
      <View testID={testID} style={{ width, height }}>
        <MediaStatus state={active ? "loading" : "idle"} />
      </View>
    );
  }
  if (error || !uri) {
    return (
      <View testID={testID} style={{ width, height }}>
        <MediaStatus state="error" />
      </View>
    );
  }
  return (
    <View testID={testID} style={{ width, height }}>
      <VideoView
        player={player}
        nativeControls
        contentFit="contain"
        fullscreenOptions={{ enable: true }}
        allowsPictureInPicture={false}
        accessibilityLabel={attachment.filename}
        style={{ width, height }}
      />
    </View>
  );
}
