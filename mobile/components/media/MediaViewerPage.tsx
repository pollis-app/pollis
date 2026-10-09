// One page of the full-screen media viewer (#1248), chosen by MIME type.

import { mediaKind } from "../../lib/media/viewer";
import { ZoomableImage } from "./ZoomableImage";
import { VideoPage } from "./VideoPage";
import { AudioPage } from "./AudioPage";
import { FilePage } from "./FilePage";
import type { MessageAttachment } from "../../types";

export function MediaViewerPage({
  attachment,
  width,
  height,
  active,
  onZoomChange,
}: {
  attachment: MessageAttachment;
  width: number;
  height: number;
  // The page on screen. Off-screen pages reset their zoom, pause playback
  // and release their named copy.
  active: boolean;
  onZoomChange: (zoomed: boolean) => void;
}) {
  const testID = `media-page-${attachment.id}`;
  switch (mediaKind(attachment.content_type)) {
    case "image":
      return (
        <ZoomableImage
          testID={testID}
          attachment={attachment}
          width={width}
          height={height}
          active={active}
          onZoomChange={onZoomChange}
        />
      );
    case "video":
      return (
        <VideoPage testID={testID} attachment={attachment} width={width} height={height} active={active} />
      );
    case "audio":
      return (
        <AudioPage testID={testID} attachment={attachment} width={width} height={height} active={active} />
      );
    default:
      return <FilePage testID={testID} attachment={attachment} width={width} height={height} />;
  }
}
