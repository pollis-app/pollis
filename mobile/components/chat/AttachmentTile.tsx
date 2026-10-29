// One attachment in a message row; every kind opens the media viewer on tap
// (#1248). Video shows its blurhash under a play glyph: expo-image cannot draw
// a video frame.

import { useMemo } from "react";
import { Pressable, Text, View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { MediaImage } from "../Media";
import { Icon } from "../icons";
import { mediaKind } from "../../lib/media/viewer";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";
import { r, semantic, type as ty } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

const IMAGE_MAX_W = 220;
function imageSize(att: MessageAttachment): { width: number; height: number } {
  if (att.width && att.height) {
    const height = Math.min(
      Math.max(Math.round((IMAGE_MAX_W * att.height) / att.width), 80),
      260,
    );
    return { width: IMAGE_MAX_W, height };
  }
  return { width: 160, height: 160 };
}

export function AttachmentTile({
  attachment,
  onOpen,
  onLongPress,
}: {
  attachment: MessageAttachment;
  onOpen?: (attachment: MessageAttachment) => void;
  // The row's own long press (message actions), so pressing an attachment
  // still reaches it.
  onLongPress?: () => void;
}) {
  const { t } = useTranslation("chat");
  const kind = mediaKind(attachment.content_type);
  const openable =
    !!onOpen && (!!attachment.object_key || !!attachment.localPreviewUri);
  const testID = `btn-attachment-${attachment.id}`;
  const placeholder = useMemo(
    () => (attachment.blurhash ? { blurhash: attachment.blurhash } : undefined),
    [attachment.blurhash],
  );

  const press = {
    testID,
    onPress: openable ? () => onOpen?.(attachment) : undefined,
    onLongPress,
    delayLongPress: LONG_PRESS_MS,
    disabled: !openable && !onLongPress,
    accessibilityRole: "button" as const,
    accessibilityState: { disabled: !openable },
  };

  if (kind === "image") {
    return (
      <Pressable
        {...press}
        accessibilityLabel={t("attachment.viewLabel", { filename: attachment.filename })}
      >
        <MediaImage
          attachment={attachment}
          contentFit="cover"
          style={{
            ...imageSize(attachment),
            borderRadius: r.md,
            borderWidth: 1,
            borderColor: semantic.hair,
          }}
        />
      </Pressable>
    );
  }

  if (kind === "video") {
    const box = imageSize(attachment);
    return (
      <Pressable
        {...press}
        accessibilityLabel={t("attachment.openLabel", { filename: attachment.filename })}
        style={{
          ...box,
          borderRadius: r.md,
          overflow: "hidden",
          backgroundColor: semantic.raised,
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {placeholder ? (
          <Image
            placeholder={placeholder}
            placeholderContentFit="cover"
            style={{ position: "absolute", top: 0, bottom: 0, start: 0, end: 0 }}
          />
        ) : null}
        <View
          style={{
            width: 44,
            height: 44,
            borderRadius: 22,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: semantic.bg,
          }}
        >
          <Icon.play size={20} color={semantic.text} />
        </View>
      </Pressable>
    );
  }

  return (
    <Pressable
      {...press}
      accessibilityLabel={t("attachment.openLabel", { filename: attachment.filename })}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: 6,
        minHeight: 44,
        paddingHorizontal: 12,
        borderRadius: r.md,
        backgroundColor: pressed && openable ? semantic.high : semantic.raised,
      })}
    >
      {kind === "audio" ? (
        <Icon.headphones size={16} color={semantic.dim} />
      ) : (
        <Icon.attach size={16} color={semantic.dim} />
      )}
      <Text numberOfLines={1} style={[ty.secondary, { maxWidth: 200 }]}>
        {attachment.filename}
      </Text>
    </Pressable>
  );
}
