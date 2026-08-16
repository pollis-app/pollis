import React, { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { shellOpen } from "../../bridge";
import { ensureProtocol, findUrls } from "../../utils/links";

// Known limitation (low priority): inline previews only fire when the URL ends in
// a recognised image/video extension. Sites like Giphy/Tenor/Imgur that serve media
// behind extension-less share URLs (e.g. giphy.com/gifs/...) won't unfurl. Pasting
// the direct .gif/.mp4 URL works, and copy-pasting the actual media into the input
// also works, so we accept the gap rather than building an OG/oEmbed unfurl service.

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

/// Hosts whose media loads without asking.
///
/// Only our own CDN: those bytes are already fetched from us, so previewing
/// them tells a third party nothing it does not already know.
const AUTO_LOAD_HOSTS = ["cdn.pollis.com"];

function isFirstParty(url: string): boolean {
  try {
    return AUTO_LOAD_HOSTS.includes(new URL(ensureProtocol(url)).hostname.toLowerCase());
  } catch {
    return false;
  }
}

export const MediaLinkUnfurl: React.FC<MediaLinkUnfurlProps> = ({ text }) => {
  const { t } = useTranslation("chat");
  const links = useMemo(() => extractMediaLinks(text), [text]);
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  // Third-party media loads only when the reader asks for it.
  //
  // Rendering `<img src>` straight from message text meant that merely OPENING
  // a message made the app fetch from a host the sender chose — handing them
  // the reader's IP address, user-agent and the moment they read it, from any
  // message including an unaccepted DM request from a stranger. That is a
  // read-receipt and a deanonymiser in one, and it defeats the relay overlay
  // entirely: the overlay hides the client's address from OUR servers while
  // this handed it to an arbitrary one.
  //
  // A click is the consent. It is deliberately per-URL and not remembered —
  // the sender picks the URL, so a blanket "always load" would be a setting the
  // attacker, not the user, gets to exploit.
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
    border: "none",
    borderRadius: "0.5rem",
    background: "transparent",
  };

  return (
    <div data-testid="media-link-unfurl" className="mt-2 flex flex-wrap gap-1">
      {visible.map((link) => {
        const href = ensureProtocol(link.url);
        const show = revealed.has(link.url) || isFirstParty(link.url);
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
              className="flex h-24 w-24 shrink-0 items-center justify-center rounded-[var(--radius-control)] border border-line bg-surface-raised px-2 text-center text-2xs text-muted hover:bg-hover hover:text-fg"
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
              <img src={href} alt="" onError={onError} style={thumbStyle} />
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
            style={{ ...thumbStyle, objectFit: "cover" }}
          />
        );
      })}
    </div>
  );
};
