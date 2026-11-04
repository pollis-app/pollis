import { useMemo, useState } from "react";
import { FlatList, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { AttachmentTile } from "../chat/AttachmentTile";
import { isRollMedia, mediaKind } from "../../lib/media/viewer";
import { semantic, type as ty, space, r } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

const COLUMNS = 3;
const GAP = space.xs;
const INSET = space.lg;

/** The Vault's media view (#1492): a virtualised grid, so only rows near the screen hold decrypted files. */
export function VaultMediaGrid({
  attachments,
  onOpen,
}: {
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
  const tile = width > 0 ? Math.floor((width - INSET * 2 - GAP * (COLUMNS - 1)) / COLUMNS) : 0;
  const box = useMemo(() => ({ width: tile, height: tile }), [tile]);

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
      data={tile > 0 ? media : []}
      keyExtractor={(a) => a.id}
      numColumns={COLUMNS}
      columnWrapperStyle={{ gap: GAP }}
      contentContainerStyle={{ gap: GAP, paddingHorizontal: INSET, paddingBottom: space.xxxl }}
      initialNumToRender={COLUMNS * 6}
      maxToRenderPerBatch={COLUMNS * 4}
      windowSize={5}
      // The chat's own tile (#1568 cover, blurhash, radius), so a picture looks the same in both views.
      renderItem={({ item }) => (
        <AttachmentTile
          attachment={item}
          onOpen={onOpen}
          box={box}
          testID={`tile-vault-media-${item.id}`}
        />
      )}
      ListFooterComponent={
        files.length > 0 ? (
          <View style={{ paddingTop: space.xl, gap: space.sm }}>
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
