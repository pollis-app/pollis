// A received sender-generated preview (#1987), inside the message. Drawn only when its URL is
// exactly a link in the body; the domain line and the tap both use that body link.

import { useMemo } from "react";
import { MediaImage } from "../Media";
import { LinkPreviewCard } from "./LinkPreviewCard";
import { matchPreviewLink } from "../../lib/linkPreview";
import { scanText } from "../../../design/messageFormat.ts";
import { tapLink } from "../../lib/openLink";
import type { MessageLinkPreview as Preview } from "../../types";

export function MessageLinkPreview({
  text,
  preview,
  onLongPress,
}: {
  text: string;
  preview: Preview | null | undefined;
  onLongPress?: () => void;
}) {
  // A link inside a code block is not a link (#2142).
  const link = useMemo(() => matchPreviewLink(scanText(text), preview), [text, preview]);
  if (!preview || !link) {
    return null;
  }
  return (
    <LinkPreviewCard
      testID="message-link-preview"
      host={link.host}
      title={preview.title}
      description={preview.description}
      thumb={
        preview.image ? (
          // Loaded exactly like an image attachment of this conversation; blurhash until then.
          <MediaImage
            key={preview.image.content_hash || preview.image.id}
            attachment={preview.image}
            style={{ width: "100%", height: "100%", borderRadius: 0 }}
          />
        ) : undefined
      }
      onPress={() => tapLink(link.href)}
      onLongPress={onLongPress}
    />
  );
}
