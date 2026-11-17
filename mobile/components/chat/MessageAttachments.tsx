// A message's attachments (#1970, desktop's MessageItem): one picture or video at its own shape, several
// as a 2-up grid of equal squares, then audio and files as a list. Rules in `lib/media/messageAttachments.ts`.

import { useMemo } from "react";
import { View } from "react-native";
import { AttachmentTile } from "./AttachmentTile";
import { messageAttachmentLayout } from "../../lib/media/messageAttachments";
import type { MessageAttachment } from "../../types";

// Desktop's `gap-1`.
const GAP = 4;
// Two cells of about 150pt: a phone's message column holds it, a tablet's does not stretch it.
const GRID_MAX_W = 304;

export function MessageAttachments({
  attachments,
  onOpen,
  onLongPress,
}: {
  attachments: MessageAttachment[];
  onOpen?: (attachment: MessageAttachment) => void;
  onLongPress?: () => void;
}) {
  const layout = useMemo(() => messageAttachmentLayout(attachments), [attachments]);
  const tile = (attachment: MessageAttachment, fill = false) => (
    <AttachmentTile
      key={attachment.id}
      attachment={attachment}
      onOpen={onOpen}
      onLongPress={onLongPress}
      fill={fill}
    />
  );

  return (
    <View style={{ gap: 6, marginTop: 4, alignItems: "flex-start" }}>
      {layout.gridRows ? (
        <View testID="message-media-grid" style={{ width: "100%", maxWidth: GRID_MAX_W, gap: GAP }}>
          {layout.gridRows.map((row) => (
            <View key={row[0].id} style={{ flexDirection: "row", gap: GAP }}>
              {row.map((attachment) => (
                <View key={attachment.id} style={{ flex: 1, aspectRatio: 1 }}>
                  {tile(attachment, true)}
                </View>
              ))}
              {/* An odd last picture stays one cell wide. */}
              {row.length === 1 ? <View style={{ flex: 1 }} /> : null}
            </View>
          ))}
        </View>
      ) : (
        layout.media.map((attachment) => tile(attachment))
      )}
      {layout.files.map((attachment) => tile(attachment))}
    </View>
  );
}
