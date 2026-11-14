// One attachment in a message row or a Vault grid cell; every kind opens the viewer (#1248). A sensitive one (#1568) is covered until a tap reveals it.

import { useMemo, type ReactNode } from "react";
import { observer } from "mobx-react-lite";
import { Pressable, StyleSheet, Text, View } from "react-native";
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
import { useLocalVideoPoster } from "../../hooks/useLocalVideoPoster";
import { useVideoPoster } from "../../hooks/useVideoPoster";
import { peekMediaUri } from "../../lib/media/cache";
import { videoPosterPlan } from "../../lib/media/videoPoster";
import { posterUri } from "../../lib/messageContent";


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
  box: boxOverride,
  testID: testIDOverride,
}: {
  attachment: MessageAttachment;
  // Absent with no conversation context.
  onOpen?: (attachment: MessageAttachment) => void;
  // The row's long press, so pressing an attachment still reaches it.
  onLongPress?: () => void;
  // A fixed frame for a picture or video (the Vault's grid); else sized from the attachment.
  box?: { width: number; height: number };
  testID?: string;
}) {
  useThemeRerender();
  const { t } = useTranslation("chat");
  const kind = mediaKind(attachment.content_type);
  const sensitivity = useSensitiveReveal(attachment);
  const openable =
    !!onOpen && (!!attachment.object_key || !!attachment.localPreviewUri);
  const testID = testIDOverride ?? `btn-attachment-${attachment.id}`;
  const visualBox = boxOverride ?? imageSize(attachment);

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
  const covered = visual && tileTap(attachment, sensitivity.revealed) === "reveal";

  // A video's still frame (#1837), in `videoPosterPlan`'s order.
  const plan =
    kind === "video" && !covered
      ? videoPosterPlan(attachment, !!(attachment.object_key && peekMediaUri(attachment.content_hash)))
      : "none";
  // A pending send's own frame, decoded from its file each time (it has no content hash yet).
  const decoded = useVideoPoster(plan === "pending" ? attachment.localPreviewUri ?? null : null);
  // No `pv` (an older sender): the poster saved on this device (#1894), else decoded once and saved.
  const local = useLocalVideoPoster(
    attachment,
    kind === "video" && !covered && plan !== "pv" && plan !== "pending",
    plan === "legacy",
  );
  const posterSource =
    plan === "pv" && attachment.poster
      ? posterUri(attachment.poster)
      : decoded
        ? posterUri(decoded)
        : local?.jpeg
          ? posterUri(local.jpeg)
          : null;
  const blurhash = attachment.blurhash ?? local?.blurhash ?? undefined;
  const placeholder = useMemo(() => (blurhash ? { blurhash } : undefined), [blurhash]);

  if (covered) {
    const box = visualBox;
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
            ...visualBox,
            borderRadius: r.md,
            borderWidth: 1,
            borderColor: semantic.hair,
          }}
        />
      </Pressable>
    );
  }

  if (kind === "video") {
    const box = visualBox;
    const posterKind = posterSource ? (plan === "pv" ? "pv" : "local") : placeholder ? "blurhash" : "placeholder";
    return withHide(
      <View style={box}>
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
        {posterSource ? (
          <Image
            source={{ uri: posterSource }}
            placeholder={placeholder}
            placeholderContentFit="cover"
            contentFit="cover"
            // Decrypted content: memory only, never expo-image's disk cache.
            cachePolicy="memory"
            style={StyleSheet.absoluteFill}
            accessible={false}
          />
        ) : placeholder ? (
          <Image
            placeholder={placeholder}
            placeholderContentFit="cover"
            style={StyleSheet.absoluteFill}
          />
        ) : null}
        <PlayBadge size={44} overFrame={!!posterSource || !!placeholder} />
      </Pressable>
      {/* What the tile drew, for Maestro (iOS hides ids inside the accessible button). */}
      <View
        testID={`video-poster-${posterKind}`}
        collapsable={false}
        pointerEvents="none"
        style={StyleSheet.absoluteFill}
      />
      </View>
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
