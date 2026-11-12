import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { errorMessage } from "../../utils/errorMessage";
import { formatFileSize } from "../../utils/format";
import { horizontalArrowStep } from "../../utils/direction";
import { stepRoll } from "../../utils/mediaRoll";
import {
  announceMediaLoaded,
  peekMediaUrl,
  saveAttachmentAs,
  subscribeMediaResolved,
} from "../../services/r2-upload";
import { useSensitiveReveal } from "../../hooks/useSensitiveReveal";
import { useResolveMediaUrl } from "../../hooks/useResolveMediaUrl";
import { Button } from "../ui/Button";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { PressButton } from "../ui/PressButton";
import { SensitiveCover } from "../Media/SensitiveCover";
import type { MessageAttachment } from "../../types";

const lightboxBtnStyle: React.CSSProperties = {
  border: "2px solid transparent",
  borderRadius: 4,
  cursor: "pointer",
  padding: "2px 8px",
};

const mediaBox: React.CSSProperties = {
  maxWidth: "90vw",
  maxHeight: "85vh",
  cursor: "default",
  borderRadius: "1rem",
};

interface MediaSpotlightProps {
  /** The message's images and videos (`spotlightRoll`); one item draws no arrows. */
  items: MessageAttachment[];
  startIndex: number;
  /** The opened card's URL, which may be a pending send's local preview. */
  openedUrl: string;
  onClose: () => void;
}

/** The image/video spotlight (#1788): prev/next, ← / → and a position over one message's media, wrapping as mobile does. */
export const MediaSpotlight: React.FC<MediaSpotlightProps> = ({
  items,
  startIndex,
  openedUrl,
  onClose,
}) => {
  const { t } = useTranslation("vault");
  const rootRef = useRef<HTMLDivElement>(null);
  const [index, setIndex] = useState(startIndex);
  const multiple = items.length > 1;
  const currentIndex = Math.min(index, items.length - 1);
  const current = items[currentIndex];
  const openedId = items[startIndex]?.id;

  const step = useCallback(
    (delta: number) => {
      setIndex((i) => stepRoll(Math.min(i, items.length - 1), delta, items.length));
    },
    [items.length],
  );

  // Bound only while the spotlight is open; capture phase so the log's arrow nav never also moves.
  useEffect(() => {
    if (!multiple) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
        return;
      }
      const delta = horizontalArrowStep(event.key, rootRef.current);
      if (delta === 0) {
        return;
      }
      event.preventDefault();
      event.stopImmediatePropagation();
      step(delta);
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [multiple, step]);

  if (!current) {
    return null;
  }

  return (
    <div
      ref={rootRef}
      data-testid="media-spotlight"
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 9999,
        background: "rgba(0,0,0,0.92)",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        cursor: "zoom-out",
      }}
      onClick={onClose}
    >
      <SpotlightStage
        key={current.id}
        attachment={current}
        initialUrl={current.id === openedId ? openedUrl : null}
        position={
          multiple ? t("media.position", { index: currentIndex + 1, total: items.length }) : null
        }
        onClose={onClose}
      />
      {multiple && (
        <>
          <div
            className="absolute start-4 top-1/2 -translate-y-1/2"
            style={{ cursor: "default" }}
            onClick={(e) => e.stopPropagation()}
          >
            <Button
              data-testid="media-spotlight-prev"
              activateOn="press"
              size="sm"
              aria-label={t("media.previous")}
              onClick={() => step(-1)}
            >
              <ChevronLeft size={20} aria-hidden="true" className="rtl-mirror" />
            </Button>
          </div>
          <div
            className="absolute end-4 top-1/2 -translate-y-1/2"
            style={{ cursor: "default" }}
            onClick={(e) => e.stopPropagation()}
          >
            <Button
              data-testid="media-spotlight-next"
              activateOn="press"
              size="sm"
              aria-label={t("media.next")}
              onClick={() => step(1)}
            >
              <ChevronRight size={20} aria-hidden="true" className="rtl-mirror" />
            </Button>
          </div>
        </>
      )}
    </div>
  );
};

interface SpotlightStageProps {
  attachment: MessageAttachment;
  initialUrl: string | null;
  position: string | null;
  onClose: () => void;
}

/** One item: keyed by id, so its URL, failure and save state start fresh on every step. */
const SpotlightStage = observer(({ attachment, initialUrl, position, onClose }: SpotlightStageProps) => {
  const { t } = useTranslation("chat");
  // Sensitive media (#1568): a neighbour still covered shows its cover; a click reveals it.
  const sensitivity = useSensitiveReveal(attachment);
  const isVideo = attachment.content_type.startsWith("video/");
  const [url, setUrl] = useState<string | null>(
    () =>
      initialUrl ??
      attachment.localPreviewUrl ??
      (attachment.object_key ? peekMediaUrl(attachment) : null),
  );
  const [failed, setFailed] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<"idle" | "downloading" | "done">("idle");
  const [saveError, setSaveError] = useState<string | null>(null);

  useResolveMediaUrl(attachment, !url && !failed && !!attachment.object_key, setUrl, () => setFailed(true));

  // #1826: "failed to load" is never sticky; the first resolve or draw of this file anywhere clears it.
  useEffect(() => {
    if (!failed || !attachment.object_key) {
      return;
    }
    return subscribeMediaResolved(attachment.content_hash, (resolved) => {
      setUrl(resolved);
      setFailed(false);
    });
  }, [failed, attachment.object_key, attachment.content_hash]);

  // A drawn file wakes every row still failing on it.
  const announceLoaded = () => {
    if (url && attachment.object_key && url !== attachment.localPreviewUrl) {
      announceMediaLoaded(attachment.content_hash, url);
    }
  };

  const handleDownload = async () => {
    if (downloadStatus !== "idle") {
      return;
    }
    setDownloadStatus("downloading");
    setSaveError(null);
    try {
      const saved = await saveAttachmentAs(attachment);
      setDownloadStatus(saved ? "done" : "idle");
      if (saved) {
        setTimeout(() => setDownloadStatus("idle"), 2000);
      }
    } catch (err) {
      setSaveError(errorMessage(err, t("attachment.failedToDownload")));
      setDownloadStatus("idle");
    }
  };

  const stopClick = (e: React.MouseEvent) => e.stopPropagation();

  let media: React.ReactNode;
  if (sensitivity.hidden) {
    media = (
      <PressButton
        data-testid="media-spotlight-cover"
        activateOn="click"
        onClick={(e) => {
          e.stopPropagation();
          sensitivity.reveal();
        }}
        aria-label={t("attachment.sensitiveReveal", { filename: attachment.filename })}
        className="relative block h-[60vh] w-[60vh] max-w-[90vw] overflow-hidden border-0 p-0"
        style={{ borderRadius: "1rem", cursor: "pointer" }}
      >
        <SensitiveCover blurhash={attachment.blurhash} />
      </PressButton>
    );
  } else if (url && !failed && isVideo) {
    media = (
      <video
        data-testid="media-spotlight-media"
        autoFocus
        src={url}
        controls
        style={mediaBox}
        onClick={stopClick}
        onLoadedData={announceLoaded}
      />
    );
  } else if (url && !failed) {
    media = (
      <img
        data-testid="media-spotlight-media"
        src={url}
        alt={attachment.filename}
        style={{ ...mediaBox, objectFit: "contain" }}
        onClick={stopClick}
        onLoad={announceLoaded}
        onError={() => setFailed(true)}
      />
    );
  } else {
    media = (
      <div
        data-testid="media-spotlight-placeholder"
        className="flex items-center justify-center p-8 text-sm font-mono text-muted"
        style={{ cursor: "default" }}
        onClick={stopClick}
      >
        {failed ? t("attachment.failedToLoad") : <LoadingSpinner size="sm" />}
      </div>
    );
  }

  return (
    <>
      {media}
      <div
        className="flex items-center gap-3 mt-3"
        style={{ cursor: "default" }}
        onClick={stopClick}
      >
        {position && (
          <span data-testid="media-spotlight-position" className="text-xs font-mono text-fg">
            {position}
          </span>
        )}
        <span className="text-xs font-mono text-muted">{attachment.filename}</span>
        {attachment.file_size > 0 && (
          <span className="text-xs font-mono text-muted">
            {formatFileSize(attachment.file_size)}
          </span>
        )}
        {saveError && <span className="text-xs font-mono text-muted">{saveError}</span>}
        <button
          onClick={handleDownload}
          disabled={downloadStatus !== "idle" || !attachment.object_key}
          className="text-xs font-mono transition-colors text-accent bg-transparent enabled:hover:bg-accent enabled:hover:text-black focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-1 focus:ring-offset-black flex items-center gap-1"
          style={{ ...lightboxBtnStyle, opacity: downloadStatus !== "idle" ? 1 : undefined }}
        >
          {downloadStatus === "downloading" ? (
            <>[ {t("attachment.lightboxFetch")} <LoadingSpinner size="sm" /> ]</>
          ) : downloadStatus === "done" ? (
            t("attachment.lightboxDone")
          ) : (
            t("attachment.lightboxDownload")
          )}
        </button>
        <button
          onClick={onClose}
          className="text-xs font-mono transition-colors text-dim bg-transparent hover:bg-accent hover:text-black focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-1 focus:ring-offset-black"
          style={lightboxBtnStyle}
        >
          {t("attachment.lightboxClose")}
        </button>
      </div>
    </>
  );
});
