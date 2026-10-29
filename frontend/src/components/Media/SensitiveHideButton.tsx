import React from "react";
import { useTranslation } from "react-i18next";
import { Eye } from "lucide-react";

/**
 * The top-right eye on a revealed sensitive attachment (#1568): one click
 * blurs it again, as often as the user likes. The open eye says what is true
 * now — the media is showing — exactly as the composer's toggle does.
 *
 * A sibling of the thumb, never inside it: the thumb is itself a button, and
 * a press here must not also open the viewer.
 */
export const SensitiveHideButton: React.FC<{
  filename: string;
  onHide: () => void;
}> = ({ filename, onHide }) => {
  const { t } = useTranslation("chat");
  return (
    <button
      type="button"
      data-testid="sensitive-hide"
      onClick={(e) => {
        e.stopPropagation();
        onHide();
      }}
      aria-label={t("attachment.sensitiveHide", { filename })}
      title={t("attachment.sensitiveHide", { filename })}
      className="absolute end-1 top-1 z-10 flex h-6 w-6 items-center justify-center rounded bg-black/60 text-white hover:bg-black/80 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
    >
      <Eye size={14} aria-hidden="true" />
    </button>
  );
};
