import { useSyncExternalStore } from "react";
import { View } from "react-native";
import { avatarTagsShowing, avatarTagsVersion, subscribeAvatarTags } from "../lib/avatarRenderState";

// Full-size and under the content, like ThemeProbe / EmojiProbe.
const PROBE = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

// Invisible e2e hooks, drawn by <Screen>: one view per tag a tagged `UserAvatar` draws
// (`<testID>-photo`, `presence-online|away|offline-<testID>`). A message or list row is one
// accessible element on iOS, so the avatar's own testIDs never reach Maestro there; these do.
// Presence tags lead with `presence-`: every user avatar draws one, and a probe is full-screen,
// so a tag a row selector (`row-message-.*`) matched would take that row's taps.
export function AvatarProbe() {
  useSyncExternalStore(subscribeAvatarTags, avatarTagsVersion);
  const tags = avatarTagsShowing();
  if (tags.length === 0) {
    return null;
  }
  return (
    <>
      {tags.map((tag) => (
        <View key={tag} testID={tag} collapsable={false} pointerEvents="none" style={PROBE} />
      ))}
    </>
  );
}
