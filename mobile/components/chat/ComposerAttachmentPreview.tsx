// One pending attachment card in the composer (#1585); a marked sensitive card (#1568) shows its blurhash and opens nothing.

import { useEffect, useRef, type ReactNode } from "react";
import { Pressable, Text, View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { PlayBadge } from "../media/PlayBadge";
import { AttachmentToolbar, TOOLBAR_GLYPH, type AttachmentToolbarAction } from "./AttachmentToolbar";
import { SensitiveCover } from "./SensitiveCover";
import { useVideoPoster } from "../../hooks/useVideoPoster";
import {
  COMPOSER_PREVIEW_SIZE,
  canMarkSensitive,
  cardOpens,
  composerAttachmentTestID,
  sensitiveToggleTestID,
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
  onChange,
}: {
  attachment: PickedAttachment;
  index: number;
  onRemove: (id: string) => void;
  onOpen?: (attachment: PickedAttachment) => void;
  // Absent where the composer cannot mark.
  onChange?: (id: string, patch: Partial<PickedAttachment>) => void;
}) {
  const { t } = useTranslation("chat");
  const { t: tc } = useTranslation("common");
  const kind = previewKind(attachment.mimeType);
  const sensitive = attachment.sensitive === true;
  const openable = !!onOpen && cardOpens(attachment);
  const poster = useVideoPoster(kind === "video" ? attachment.uri : null);

  // The cover's blurhash, once; the callback via a ref (screens pass a fresh arrow).
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const hashSource = kind === "video" ? poster : kind === "image" ? attachment.uri : null;
  useEffect(() => {
    if (!sensitive || attachment.blurhash || !hashSource || !onChangeRef.current) {
      return;
    }
    let active = true;
    Image.generateBlurhashAsync(hashSource as Parameters<typeof Image.generateBlurhashAsync>[0], [4, 3])
      .then((hash) => {
        if (active && hash) {
          onChangeRef.current?.(attachment.id, { blurhash: hash });
        }
      })
      .catch(() => {
        // No blurhash: the cover is a plain surface with the eye.
      });
    return () => {
      active = false;
    };
  }, [sensitive, attachment.blurhash, attachment.id, hashSource]);

  let body: ReactNode;
  if (sensitive) {
    body = <SensitiveCover blurhash={attachment.blurhash} testID={`sensitive-cover-composer-${index}`} />;
  } else if (kind === "image") {
    body = (
      <Image
        source={{ uri: attachment.uri }}
        contentFit="cover"
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
          ...(onChange && canMarkSensitive(attachment.mimeType)
            ? [
                {
                  key: "sensitive",
                  testID: sensitiveToggleTestID(index),
                  // The accent glyph is the "on" state.
                  accessibilityLabel: tc(
                    sensitive ? "composer.unmarkSensitive" : "composer.markSensitive",
                    { name: attachment.name },
                  ),
                  selected: sensitive,
                  icon: (color: string) =>
                    sensitive ? (
                      <Icon.eyeOff size={TOOLBAR_GLYPH} color={color} />
                    ) : (
                      <Icon.eye size={TOOLBAR_GLYPH} color={color} />
                    ),
                  onPress: () => onChange(attachment.id, { sensitive: !sensitive }),
                } satisfies AttachmentToolbarAction,
              ]
            : []),
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
        importantForAccessibility="no"
        accessibilityElementsHidden
        style={[ty.meta, { marginTop: 4, color: semantic.dim }]}
      >
        {attachment.name}
      </Text>
    </View>
  );
}
