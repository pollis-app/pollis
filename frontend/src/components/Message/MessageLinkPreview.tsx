import React, { useMemo, useState } from "react";
import { peekMediaUrl } from "../../services/r2-upload";
import { useResolveMediaUrl } from "../../hooks/useResolveMediaUrl";
import { matchPreviewLink } from "../../utils/linkPreview";
import { openMessageLink } from "../ui/LinkifiedText";
import { BlurhashCanvas } from "../Media/BlurhashCanvas";
import { LinkPreviewCard } from "./LinkPreviewCard";
import type { MessageAttachment, MessageLinkPreview as Preview } from "../../types";

interface MessageLinkPreviewProps {
  /** The message body as shown; the preview must name one of its links exactly. */
  text: string;
  preview: Preview | null | undefined;
}

/** The thumbnail, loaded exactly like an image attachment of this conversation; blurhash until then. */
const PreviewThumb: React.FC<{ image: MessageAttachment }> = ({ image }) => {
  const [url, setUrl] = useState<string | null>(
    () => image.localPreviewUrl ?? (image.object_key ? peekMediaUrl(image) : null),
  );
  const [failed, setFailed] = useState(false);
  useResolveMediaUrl(image, !url && !failed && !!image.object_key, setUrl, () => setFailed(true));

  if (url && !failed) {
    return (
      <img
        src={url}
        alt=""
        draggable={false}
        onError={() => setFailed(true)}
        className="h-full w-full object-cover"
      />
    );
  }
  if (image.blurhash) {
    return <BlurhashCanvas hash={image.blurhash} />;
  }
  return null;
};

/**
 * A received sender-generated preview (#1987). Drawn only when its URL is exactly a link in the
 * body; the domain line and the click both use that body link, never the preview's own data.
 */
export const MessageLinkPreview: React.FC<MessageLinkPreviewProps> = React.memo(({ text, preview }) => {
  const link = useMemo(() => matchPreviewLink(text, preview), [text, preview]);
  if (!preview || !link) {
    return null;
  }
  return (
    <LinkPreviewCard
      testId="message-link-preview"
      host={link.host}
      title={preview.title}
      description={preview.description}
      thumb={
        preview.image ? (
          <PreviewThumb key={preview.image.content_hash || preview.image.id} image={preview.image} />
        ) : undefined
      }
      href={link.href}
      onOpen={() => openMessageLink(link.href)}
    />
  );
});

MessageLinkPreview.displayName = "MessageLinkPreview";
