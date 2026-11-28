import React from "react";
import { useRowT } from "../../i18n/useRowT";
import { X } from "lucide-react";
import { IconButton } from "../ui/IconButton";
import { FOCUS_RING } from "../ui/focusRing";

interface LinkPreviewCardProps {
  /** The body link's host (never from the preview); pinned LTR. */
  host: string;
  /** Plain text only: never parsed as HTML. */
  title: string;
  description?: string | null;
  /** The square thumbnail; absent = no thumbnail column. */
  thumb?: React.ReactNode;
  /** A received card opens its body link (`href`) on click. */
  href?: string;
  onOpen?: () => void;
  /** A composer card's ×. */
  onRemove?: () => void;
  /** Fetching: the same box, with the text still to come. */
  loading?: boolean;
  testId?: string;
}

// The card's box is fixed (one host line, one title line, two description lines), so a
// thumbnail or a late description never moves the message under it.
const CARD =
  "flex h-[5.25rem] w-full max-w-md items-center gap-3 overflow-hidden rounded-panel border border-line bg-surface-raised py-2 pe-2 ps-3 text-start";

/** A sender-generated link preview (#1987), compact Slack layout: text start, thumbnail end. */
export const LinkPreviewCard: React.FC<LinkPreviewCardProps> = ({
  host,
  title,
  description,
  thumb,
  href,
  onOpen,
  onRemove,
  loading = false,
  testId,
}) => {
  const t = useRowT("chat");

  const text = (
    <div className="flex min-w-0 flex-1 flex-col self-stretch">
      <bdi dir="ltr" className="truncate text-meta leading-4 text-muted">
        {host}
      </bdi>
      {loading ? (
        <span className="truncate text-secondary leading-5 text-muted">{t("linkPreview.loading")}</span>
      ) : (
        <>
          <span dir="auto" className="truncate text-body font-semibold leading-5 text-fg">
            {title}
          </span>
          {description ? (
            <span dir="auto" className="line-clamp-2 text-secondary leading-4 text-dim">
              {description}
            </span>
          ) : null}
        </>
      )}
    </div>
  );

  const thumbBox = thumb ? (
    <div className="h-16 w-16 flex-shrink-0 overflow-hidden rounded-chip bg-surface-high">{thumb}</div>
  ) : null;

  if (href && onOpen) {
    return (
      <a
        data-testid={testId}
        href={href}
        title={href}
        onClick={(e) => {
          e.preventDefault();
          onOpen();
        }}
        className={`${CARD} ${FOCUS_RING} mt-2 cursor-pointer no-underline hover:bg-surface-high`}
      >
        {text}
        {thumbBox}
      </a>
    );
  }

  return (
    <div
      data-testid={testId}
      aria-busy={loading}
      className={CARD}
    >
      {text}
      {thumbBox}
      {onRemove ? (
        <IconButton
          data-testid="link-preview-remove"
          onClick={onRemove}
          aria-label={t("linkPreview.remove")}
          title={t("linkPreview.remove")}
          className="flex-shrink-0 self-start"
        >
          <X size={16} aria-hidden="true" />
        </IconButton>
      ) : null}
    </div>
  );
};
