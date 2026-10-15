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

import { useMemo, useState } from "react";
import { View, Text } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
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
  const { t } = useTranslation("mobile");
  // While an optimistic send is still uploading there's no object key to
  // fetch — render the local preview directly and skip the transport.
  // The same holds for a row the outbox has not uploaded yet (#1335), which
  // has no local preview either — it waits on its blurhash.
  const isPending = !attachment.object_key;
  const { uri, error } = useMediaUri(isPending ? null : attachment);

  const displayUri = isPending ? attachment.localPreviewUri ?? null : uri;

  // expo-image takes blurhash as a placeholder source. Memoize so the
  // placeholder object identity is stable across renders.
  const placeholder = useMemo(
    () => (attachment.blurhash ? { blurhash: attachment.blurhash } : undefined),
    [attachment.blurhash],
  );

  // The URI expo-image last finished drawing. Drives the testID below, so the
  // Maestro tier can tell a decrypted, rendered image from a blurhash
  // placeholder that never resolved.
  const [renderedUri, setRenderedUri] = useState<string | null>(null);
  const rendered = !!displayUri && renderedUri === displayUri;

  if (error) {
    return (
      <View
        testID="media-unavailable"
        style={[
          {
            borderRadius: r.md,
            backgroundColor: semantic.raised,
            alignItems: "center",
            justifyContent: "center",
            padding: 12,
          },
          style,
        ]}
      >
        <Text style={[ty.meta, { textAlign: "center" }]}>{t("ui.imageUnavailable")}</Text>
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
      // Styles are inline (not StyleSheet.create) so the theme getters are
      // read on every render and follow a live accent change.
      style={[{ borderRadius: r.md, backgroundColor: semantic.raised }, style]}
      accessibilityLabel={attachment.filename}
      testID={rendered ? "media-image-rendered" : "media-image-loading"}
      onLoad={() => setRenderedUri(displayUri)}
    />
  );
}
