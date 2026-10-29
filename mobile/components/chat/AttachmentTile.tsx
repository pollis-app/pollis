// One attachment in a message row; every kind opens the viewer (#1248). A sensitive one (#1568) is covered until a tap reveals it.

import { useMemo, type ReactNode } from "react";
import { observer } from "mobx-react-lite";
import { Pressable, Text, View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { MediaImage } from "../Media";
import { Icon } from "../icons";
import { PlayBadge } from "../media/PlayBadge";
import { mediaKind } from "../../lib/media/viewer";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";
import { r, semantic, type as ty } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";
import { useThemeRerender } from "../theme";
import { useSensitiveReveal } from "../../hooks/useSensitiveReveal";
import { tileTap } from "../../lib/sensitiveMedia";
import { SensitiveCover, SensitiveHideButton } from "./SensitiveCover";

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

export const AttachmentTile = observer(function AttachmentTile({
  attachment,
  onOpen,
  onLongPress,
}: {
  attachment: MessageAttachment;
  // Absent with no conversation context.
  onOpen?: (attachment: MessageAttachment) => void;
  // The row's long press, so pressing an attachment still reaches it.
  onLongPress?: () => void;
}) {
  useThemeRerender();
  const { t } = useTranslation("chat");
  const kind = mediaKind(attachment.content_type);
  const sensitivity = useSensitiveReveal(attachment);
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

  const visual = kind === "image" || kind === "video";
  if (visual && tileTap(attachment, sensitivity.revealed) === "reveal") {
    const box = imageSize(attachment);
    return (
      <Pressable
        testID={testID}
        onPress={sensitivity.reveal}
        onLongPress={onLongPress}
        delayLongPress={LONG_PRESS_MS}
        accessibilityRole="button"
        accessibilityLabel={t("attachment.sensitiveReveal", { filename: attachment.filename })}
        style={{ ...box, borderRadius: r.md, overflow: "hidden" }}
      >
        <SensitiveCover blurhash={attachment.blurhash} testID={`sensitive-cover-${attachment.id}`} />
      </Pressable>
    );
  }
  // The hide button is a sibling, so hiding never opens the viewer.
  const withHide = (tile: ReactNode) =>
    sensitivity.sensitive ? (
      <View style={{ alignSelf: "flex-start" }}>
        {tile}
        <SensitiveHideButton
          filename={attachment.filename}
          onHide={sensitivity.hide}
          testID={`btn-sensitive-hide-${attachment.id}`}
        />
      </View>
    ) : (
      tile
    );

  if (kind === "image") {
    return withHide(
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
    return withHide(
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
        <PlayBadge size={44} overFrame={!!placeholder} />
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
});
