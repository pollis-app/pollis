import { useMemo, useState } from "react";
import { FlatList, Pressable, Text, View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { MediaImage } from "../Media";
import { Icon } from "../icons";
import { PlayBadge } from "../media/PlayBadge";
import { AttachmentTile } from "../chat/AttachmentTile";
import { isRollMedia, mediaKind } from "../../lib/media/viewer";
import { semantic, type as ty, space, r } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

const COLUMNS = 3;
const GAP = 2;

/**
 * The Vault's media view — desktop's MediaGalleryView: images and videos as a
 * Photos-style grid (newest first), every other file as a list below. A tap
 * opens the full-screen viewer, which walks the same images and videos in
 * the same order (#1492).
 *
 * Virtualised (#1492): a vault of hundreds of photos mounts only the rows
 * near the screen, so only those decrypt, and a row scrolled far away
 * unmounts and releases (unlinks) its plaintext files.
 */
export function VaultMediaGrid({
  attachments,
  onOpen,
}: {
  // Newest first — the caller orders.
  attachments: MessageAttachment[];
  onOpen: (attachment: MessageAttachment) => void;
}) {
  const { t } = useTranslation("vault");
  const [width, setWidth] = useState(0);
  const media = useMemo(
    () => attachments.filter((a) => isRollMedia(mediaKind(a.content_type))),
    [attachments],
  );
  const files = useMemo(
    () => attachments.filter((a) => !isRollMedia(mediaKind(a.content_type))),
    [attachments],
  );
  const tile = width > 0 ? Math.floor((width - GAP * (COLUMNS - 1)) / COLUMNS) : 0;

  if (attachments.length === 0) {
    return (
      <View testID="vault-media-empty" style={{ padding: space.xxl }}>
        <Text style={[ty.secondary, { textAlign: "center" }]}>{t("media.empty")}</Text>
      </View>
    );
  }

  return (
    <FlatList
      testID="vault-media"
      style={{ flex: 1 }}
      onLayout={(e) => setWidth(e.nativeEvent.layout.width)}
      // Nothing to lay out until the width is known.
      data={tile > 0 ? media : []}
      keyExtractor={(a) => a.id}
      numColumns={COLUMNS}
      columnWrapperStyle={{ gap: GAP }}
      contentContainerStyle={{ gap: GAP, paddingBottom: space.xxxl }}
      initialNumToRender={COLUMNS * 6}
      maxToRenderPerBatch={COLUMNS * 4}
      windowSize={5}
      renderItem={({ item }) => <VaultMediaTile attachment={item} size={tile} onOpen={onOpen} />}
      ListFooterComponent={
        files.length > 0 ? (
          <View style={{ paddingHorizontal: space.lg, paddingTop: space.xl, gap: space.sm }}>
            <Text accessibilityRole="header" style={[ty.section, { color: semantic.dim }]}>
              {t("media.filesHeading")}
            </Text>
            <View style={{ gap: 6, alignItems: "flex-start", borderRadius: r.md }}>
              {files.map((attachment) => (
                <AttachmentTile key={attachment.id} attachment={attachment} onOpen={onOpen} />
              ))}
            </View>
          </View>
        ) : null
      }
    />
  );
}

function VaultMediaTile({
  attachment,
  size,
  onOpen,
}: {
  attachment: MessageAttachment;
  size: number;
  onOpen: (attachment: MessageAttachment) => void;
}) {
  const { t } = useTranslation("vault");
  return (
    <Pressable
      testID={`tile-vault-media-${attachment.id}`}
      accessibilityRole="button"
      accessibilityLabel={t("chat:attachment.viewLabel", {
        filename: attachment.filename,
      })}
      onPress={() => onOpen(attachment)}
      style={{ width: size, height: size, backgroundColor: semantic.raised }}
    >
      {mediaKind(attachment.content_type) === "image" ? (
        <MediaImage attachment={attachment} contentFit="cover" style={{ width: size, height: size }} />
      ) : (
        <View
          style={{
            width: size,
            height: size,
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          {attachment.blurhash ? (
            <Image
              placeholder={{ blurhash: attachment.blurhash }}
              placeholderContentFit="cover"
              style={{ position: "absolute", top: 0, bottom: 0, start: 0, end: 0 }}
            />
          ) : null}
          <PlayBadge size={36} overFrame={!!attachment.blurhash} />
        </View>
      )}
    </Pressable>
  );
}
