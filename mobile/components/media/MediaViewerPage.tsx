// One page of the full-screen media viewer (#1248), chosen by MIME type.
// A sensitive item the user has not revealed (#1568) — one swiped to from a
// neighbour — shows its cover instead, and a tap reveals it; the media page
// (a video's player included) is not mounted until then.

import { Pressable, View } from "react-native";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { useSensitiveReveal } from "../../hooks/useSensitiveReveal";
import { SensitiveCover } from "../chat/SensitiveCover";

import { mediaKind } from "../../lib/media/viewer";
import { ZoomableImage } from "./ZoomableImage";
import { VideoPage } from "./VideoPage";
import { AudioPage } from "./AudioPage";
import { FilePage } from "./FilePage";
import type { MessageAttachment } from "../../types";

export const MediaViewerPage = observer(function MediaViewerPage({
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
  const { t } = useTranslation("chat");
  const sensitivity = useSensitiveReveal(attachment);
  const kind = mediaKind(attachment.content_type);
  if (sensitivity.covered && (kind === "image" || kind === "video")) {
    return (
      <View style={{ width, height, alignItems: "center", justifyContent: "center" }}>
        <Pressable
          testID={`${testID}-covered`}
          onPress={sensitivity.reveal}
          accessibilityRole="button"
          accessibilityLabel={t("attachment.sensitiveReveal", { filename: attachment.filename })}
          style={{ width: Math.min(width, height) * 0.8, height: Math.min(width, height) * 0.8, overflow: "hidden" }}
        >
          <SensitiveCover blurhash={attachment.blurhash} />
        </Pressable>
      </View>
    );
  }
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
});
