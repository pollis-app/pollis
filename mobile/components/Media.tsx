// Media renderers for message attachments (issue #346).
//
// `<MediaImage>` is the mobile counterpart to the desktop `<img>` in
// MessageItem: it resolves the attachment to a local `file://` URI via
// `useMediaUri` and hands it to `expo-image`, showing the blurhash as a
// placeholder while the decrypt-to-path round-trips through Rust.
//
// All attachment rendering should go through this seam so the transport
// (file path today, possibly an expo-image cache delegate later — see the
// issue) can change without touching screens.

import { useMemo } from "react";
import { View, Text, StyleSheet } from "react-native";
import { Image } from "expo-image";
import type { ImageStyle, StyleProp } from "react-native";
import { useMediaUri } from "../hooks/useMediaUri";
import { semantic, r, type as ty } from "../theme/tokens";
import type { MessageAttachment } from "../types";

export function MediaImage({
  attachment,
  style,
  contentFit = "cover",
}: {
  attachment: MessageAttachment;
  style?: StyleProp<ImageStyle>;
  contentFit?: "cover" | "contain";
}) {
  // While an optimistic send is still uploading there's no object key to
  // fetch — render the local preview directly and skip the transport.
  const isPending = !attachment.object_key && !!attachment.localPreviewUri;
  const { uri, error } = useMediaUri(isPending ? null : attachment);

  const displayUri = isPending ? attachment.localPreviewUri ?? null : uri;

  // expo-image takes blurhash as a placeholder source. Memoize so the
  // placeholder object identity is stable across renders.
  const placeholder = useMemo(
    () => (attachment.blurhash ? { blurhash: attachment.blurhash } : undefined),
    [attachment.blurhash],
  );

  if (error) {
    return (
      <View style={[styles.fallback, style]}>
        <Text style={styles.fallbackText}>Image unavailable</Text>
      </View>
    );
  }

  return (
    <Image
      source={displayUri ? { uri: displayUri } : undefined}
      placeholder={placeholder}
      placeholderContentFit={contentFit}
      contentFit={contentFit}
      // Plaintext on disk is unlinked on unmount (see lib/media/cache), so
      // expo-image must not keep its own copy of the decrypted bytes.
      cachePolicy="none"
      transition={150}
      style={[styles.image, style]}
      accessibilityLabel={attachment.filename}
    />
  );
}

const styles = StyleSheet.create({
  image: {
    borderRadius: r.lg,
    backgroundColor: semantic.fieldBg,
  },
  fallback: {
    borderRadius: r.lg,
    borderWidth: 1,
    borderColor: semantic.hair,
    backgroundColor: semantic.fieldBg,
    alignItems: "center",
    justifyContent: "center",
    padding: 12,
  },
  fallbackText: {
    fontFamily: ty.rowSub.fontFamily,
    fontSize: 11,
    color: semantic.mute,
  },
});
