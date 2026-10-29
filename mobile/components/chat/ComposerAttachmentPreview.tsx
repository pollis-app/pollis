// One pending attachment in the composer (#1585), mobile's port of desktop's
// `AttachmentPreview` (ChatInput.tsx): a square thumbnail card with the
// filename underneath and the toolbar at its top-end corner. An image shows
// itself, a video its first frame under a play glyph, anything else a file
// card. Tapping an image or video opens the full-screen viewer on the
// picked file, before anything is sent.

import type { ReactNode } from "react";
import { Pressable, Text, View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { PlayBadge } from "../media/PlayBadge";
import { AttachmentToolbar, TOOLBAR_GLYPH } from "./AttachmentToolbar";
import { useVideoPoster } from "../../hooks/useVideoPoster";
import {
  COMPOSER_PREVIEW_SIZE,
  canOpenPreview,
  composerAttachmentTestID,
  previewKind,
  removeAttachmentTestID,
} from "../../lib/composerAttachments";
import type { PickedAttachment } from "../../lib/attachments";
import { r, semantic, type as ty } from "../../theme/tokens";

const SIZE = COMPOSER_PREVIEW_SIZE;

export function ComposerAttachmentPreview({
  attachment,
  index,
  onRemove,
  onOpen,
}: {
  attachment: PickedAttachment;
  // Position in the strip; names the test ids.
  index: number;
  onRemove: (id: string) => void;
  onOpen?: (attachment: PickedAttachment) => void;
}) {
  const { t } = useTranslation("chat");
  const { t: tc } = useTranslation("common");
  const kind = previewKind(attachment.mimeType);
  const openable = !!onOpen && canOpenPreview(attachment.mimeType);
  const poster = useVideoPoster(kind === "video" ? attachment.uri : null);

  let body: ReactNode;
  if (kind === "image") {
    body = (
      <Image
        source={{ uri: attachment.uri }}
        contentFit="cover"
        // The picker's own file; nothing decrypted, but no second copy either.
        cachePolicy="none"
        style={{ width: SIZE, height: SIZE }}
        accessible={false}
      />
    );
  } else if (kind === "video") {
    body = (
      <>
        {poster ? (
          <Image
            source={poster}
            contentFit="cover"
            style={{ position: "absolute", top: 0, bottom: 0, start: 0, end: 0 }}
            accessible={false}
          />
        ) : null}
        <PlayBadge size={44} overFrame={!!poster} />
      </>
    );
  } else {
    body =
      kind === "audio" ? (
        <Icon.headphones size={28} color={semantic.dim} />
      ) : (
        <Icon.file size={28} color={semantic.dim} />
      );
  }

  return (
    <View style={{ width: SIZE }}>
      <Pressable
        testID={composerAttachmentTestID(index)}
        onPress={openable ? () => onOpen?.(attachment) : undefined}
        disabled={!openable}
        accessibilityRole={openable ? "button" : undefined}
        accessibilityLabel={
          openable
            ? t("attachment.viewLabel", { filename: attachment.name })
            : attachment.name
        }
        style={({ pressed }) => ({
          width: SIZE,
          height: SIZE,
          borderRadius: r.md,
          overflow: "hidden",
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: pressed && openable ? semantic.high : semantic.raised,
        })}
      >
        {body}
      </Pressable>
      <AttachmentToolbar
        testID={`toolbar-attachment-${index}`}
        actions={[
          {
            key: "remove",
            testID: removeAttachmentTestID(index),
            accessibilityLabel: tc("composer.removeAttachment", { name: attachment.name }),
            icon: (color) => <Icon.close size={TOOLBAR_GLYPH} color={color} />,
            onPress: () => onRemove(attachment.id),
          },
        ]}
      />
      <Text
        numberOfLines={1}
        // The card already says the name to a screen reader.
        importantForAccessibility="no"
        accessibilityElementsHidden
        style={[ty.meta, { marginTop: 4, color: semantic.dim }]}
      >
        {attachment.name}
      </Text>
    </View>
  );
}
