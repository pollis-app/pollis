// Media renderers for message attachments (#346): every attachment renders through this seam.

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
  // No object key yet (uploading, or an outbox row, #1335): local preview or blurhash.
  const isPending = !attachment.object_key;
  const { uri, error } = useMediaUri(isPending ? null : attachment);

  const displayUri = isPending ? attachment.localPreviewUri ?? null : uri;

  const placeholder = useMemo(
    () => (attachment.blurhash ? { blurhash: attachment.blurhash } : undefined),
    [attachment.blurhash],
  );

  // Lets Maestro tell a rendered image from a blurhash that never resolved.
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
      // Plaintext is unlinked on unmount: expo-image must keep no copy.
      cachePolicy="none"
      transition={150}
      // Inline styles so the theme getters follow a live accent change.
      style={[{ borderRadius: r.md, backgroundColor: semantic.raised }, style]}
      accessibilityLabel={attachment.filename}
      testID={rendered ? "media-image-rendered" : "media-image-loading"}
      onLoad={() => setRenderedUri(displayUri)}
    />
  );
}
