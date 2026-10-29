// Media renderers for message attachments (#346). All attachment rendering
// goes through this seam so the transport can change without touching screens.

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
  // No object key yet (an optimistic send, or a row the outbox has not
  // uploaded, #1335): render the local preview, or wait on the blurhash.
  const isPending = !attachment.object_key;
  const { uri, error } = useMediaUri(isPending ? null : attachment);

  const displayUri = isPending ? attachment.localPreviewUri ?? null : uri;

  const placeholder = useMemo(
    () => (attachment.blurhash ? { blurhash: attachment.blurhash } : undefined),
    [attachment.blurhash],
  );

  // The URI expo-image last finished drawing; drives the testID so Maestro can
  // tell a rendered image from a placeholder that never resolved.
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
      // Inline (not StyleSheet.create) so theme getters follow a live accent change.
      style={[{ borderRadius: r.md, backgroundColor: semantic.raised }, style]}
      accessibilityLabel={attachment.filename}
      testID={rendered ? "media-image-rendered" : "media-image-loading"}
      onLoad={() => setRenderedUri(displayUri)}
    />
  );
}
