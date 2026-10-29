import React, { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { shellOpen } from "../../bridge";
import { ensureProtocol, findUrls } from "../../utils/links";

// Only URLs ending in a recognised image/video extension unfurl; extension-less
// share URLs (Giphy, Tenor) don't. Accepted gap: no OG/oEmbed service.

const IMAGE_EXTS = ["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg"];
const VIDEO_EXTS = ["mp4", "webm", "mov", "m4v", "ogv"];

type MediaKind = "image" | "video";

interface MediaLink {
  url: string;
  kind: MediaKind;
}

function classify(url: string): MediaKind | null {
  let pathname: string;
  try {
    pathname = new URL(ensureProtocol(url)).pathname.toLowerCase();
  } catch {
    return null;
  }
  const dot = pathname.lastIndexOf(".");
  if (dot < 0) {
    return null;
  }
  const ext = pathname.slice(dot + 1);
  if (IMAGE_EXTS.includes(ext)) {
    return "image";
  }
  if (VIDEO_EXTS.includes(ext)) {
    return "video";
  }
  return null;
}

function extractMediaLinks(text: string): MediaLink[] {
  const out: MediaLink[] = [];
  const seen = new Set<string>();
  for (const { url } of findUrls(text) ?? []) {
    if (seen.has(url)) {
      continue;
    }
    seen.add(url);
    const kind = classify(url);
    if (kind) {
      out.push({ url, kind });
    }
  }
  return out;
}

interface MediaLinkUnfurlProps {
  text: string;
}

export const MediaLinkUnfurl: React.FC<MediaLinkUnfurlProps> = ({ text }) => {
  const { t } = useTranslation("chat");
  const links = useMemo(() => extractMediaLinks(text), [text]);
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  // Every host is click-to-load, no allowlist, per URL and never remembered:
  // an automatic fetch hands a sender-chosen host the reader's IP and read time,
  // and webview requests can't ride the relay overlay. See
  // `.codesight/wiki/ui.md` → MediaLinkUnfurl.
  const [revealed, setRevealed] = useState<Set<string>>(() => new Set());

  const handleClick = useCallback((url: string) => {
    shellOpen(ensureProtocol(url));
  }, []);

  const visible = links.filter((l) => !hidden.has(l.url));
  if (visible.length === 0) {
    return null;
  }

  const thumbStyle: React.CSSProperties = {
    width: 96,
    height: 96,
    objectFit: "cover",
    display: "block",
    borderRadius: "0.5rem",
    background: "transparent",
  };

  return (
    <div data-testid="media-link-unfurl" className="mt-2 flex flex-wrap gap-1">
      {visible.map((link) => {
        const href = ensureProtocol(link.url);
        const show = revealed.has(link.url);
        if (!show) {
          return (
            <button
              key={link.url}
              type="button"
              data-testid="media-link-reveal"
              onClick={() =>
                setRevealed((prev) => new Set(prev).add(link.url))
              }
              title={t("unfurl.loadTitle", { url: href })}
              aria-label={t("unfurl.loadDescription", { url: href })}
              className="flex h-24 w-24 shrink-0 items-center justify-center rounded-control border border-line bg-surface-raised px-2 text-center text-2xs text-muted hover:bg-hover hover:text-fg"
            >
              {link.kind === "image"
                ? t("unfurl.loadImage")
                : t("unfurl.loadVideo")}
            </button>
          );
        }
        const onError = () =>
          setHidden((prev) => {
            const next = new Set(prev);
            next.add(link.url);
            return next;
          });
        if (link.kind === "image") {
          return (
            <button
              key={link.url}
              type="button"
              onClick={() => handleClick(link.url)}
              className="p-0 bg-transparent border-0 cursor-pointer flex-shrink-0"
              title={href}
              aria-label={t("unfurl.openLabel", { url: href })}
            >
              <img src={href} alt="" onError={onError} className="border-0" style={thumbStyle} />
            </button>
          );
        }
        return (
          <video
            key={link.url}
            src={href}
            controls
            preload="metadata"
            onError={onError}
            className="border-0"
            style={{ ...thumbStyle, objectFit: "cover" }}
          />
        );
      })}
    </div>
  );
};
