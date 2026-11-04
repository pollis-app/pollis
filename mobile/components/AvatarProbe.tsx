import { useSyncExternalStore } from "react";
import { View } from "react-native";
import {
  avatarPhotosShowing,
  avatarPhotosVersion,
  subscribeAvatarPhotos,
} from "../lib/avatarRenderState";

// Full-size and under the content, like ThemeProbe / EmojiProbe.
const PROBE = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

// Invisible e2e hooks, drawn by <Screen>: `<testID>-photo` for every tagged
// `UserAvatar` drawing a photo. A message or list row is one accessible element
// on iOS, so the avatar's own testID never reaches Maestro there; this does.
export function AvatarProbe() {
  useSyncExternalStore(subscribeAvatarPhotos, avatarPhotosVersion);
  const ids = avatarPhotosShowing();
  if (ids.length === 0) {
    return null;
  }
  return (
    <>
      {ids.map((id) => (
        <View
          key={id}
          testID={`${id}-photo`}
          collapsable={false}
          pointerEvents="none"
          style={PROBE}
        />
      ))}
    </>
  );
}
