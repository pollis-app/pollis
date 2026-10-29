// One attachment in a message row. Every kind opens the full-screen media
// viewer on tap (#1248) — desktop's AttachmentDisplay opens its lightbox the
// same way: images as a decrypted thumbnail, video as its blurhash under a
// play glyph (expo-image cannot draw a video frame), audio and other files
// as a named chip.
//
// A sensitive image or video (#1568) is covered — its blurhash under a
// crossed-out eye — until a tap reveals it. That tap only reveals: it never
// opens the viewer, so a video's reveal never starts it playing. A revealed
// one carries an eye in its top-right corner that covers it again.

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

// Image sizing: fixed max width, height follows the aspect ratio within
// sane bounds; unknown dimensions get a square fallback.
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
  // Absent where the row cannot open a viewer (no conversation context).
  onOpen?: (attachment: MessageAttachment) => void;
  // The row's own long press (message actions), so pressing an attachment
  // still reaches it.
  onLongPress?: () => void;
}) {
  useThemeRerender();
  const { t } = useTranslation("chat");
  const kind = mediaKind(attachment.content_type);
  const sensitivity = useSensitiveReveal(attachment);
  // An upload still in flight with no local file has nothing to show yet.
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
  // A revealed sensitive tile sits in a box with its hide button beside it —
  // a sibling, so covering it again never also opens the viewer.
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

  // Audio and other files: a named chip.
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
