import React from "react";
import { useTranslation } from "react-i18next";
import { Eye } from "lucide-react";
import { IconButton } from "../ui/IconButton";

/** Re-hides a revealed sensitive attachment (#1568). A sibling of the thumb, which is itself a button. */
export const SensitiveHideButton: React.FC<{
  filename: string;
  onHide: () => void;
}> = ({ filename, onHide }) => {
  const { t } = useTranslation("chat");
  return (
    <IconButton
      data-testid="sensitive-hide"
      onClick={(e) => {
        e.stopPropagation();
        onHide();
      }}
      aria-label={t("attachment.sensitiveHide", { filename })}
      title={t("attachment.sensitiveHide", { filename })}
      // Over the picture: a solid raised chip, so the eye reads on any image.
      className="absolute end-1 top-1 z-10 bg-surface-high text-fg hover:bg-surface-raised"
    >
      <Eye size={14} aria-hidden="true" />
    </IconButton>
  );
};
