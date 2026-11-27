import React from "react";
import { matchPreviewLink } from "../../utils/linkPreview";
import { MediaLinkUnfurl } from "./MediaLinkUnfurl";
import { MessageLinkPreview } from "./MessageLinkPreview";
import type { MessageRenderContext } from "./messageRenderContext";
import type { MessageLinkPreview as Preview } from "../../types";

interface MessageUnfurlsProps {
  /** The message body as shown. */
  text: string;
  preview: Preview | null | undefined;
  ctx: MessageRenderContext;
}

/**
 * What follows a message body wherever one is drawn (the log, the pinned list #2127): the
 * sender's preview (#1987; Modern look only, never in an unaccepted request, only for a body
 * link), then click-to-load tiles for the other media links.
 */
export const MessageUnfurls: React.FC<MessageUnfurlsProps> = ({ text, preview, ctx }) => {
  const previewLink =
    ctx.skin === "modern" && ctx.linkPreviews ? matchPreviewLink(text, preview) : null;
  return (
    <>
      {previewLink && <MessageLinkPreview text={text} preview={preview} />}
      <MediaLinkUnfurl text={text} exceptHref={previewLink?.href ?? null} />
    </>
  );
};
