import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../hooks/queries/usePreferences";
import { Button } from "../ui/Button";
import { LightboxDownloadLabel } from "./LightboxDownloadLabel";

interface LightboxActionsProps {
  status: "idle" | "downloading" | "done";
  /** Also off when there is nothing to save yet (a pending send has no object key). */
  downloadDisabled?: boolean;
  onDownload: () => void;
  onClose: () => void;
}

/** Download and Close under an open picture (#1967), shared by `AttachmentDisplay`'s viewer and `MediaSpotlight`. */
export const LightboxActions: React.FC<LightboxActionsProps> = ({ status, downloadDisabled = false, onDownload, onClose }) => {
  const { t } = useTranslation("chat");
  const isTerminal = useSkin() === "terminal";
  return (
    <>
      <Button
        variant="secondary"
        size="sm"
        onClick={onDownload}
        disabled={status !== "idle" || downloadDisabled}
        data-testid="lightbox-download"
      >
        <LightboxDownloadLabel status={status} />
      </Button>
      <Button variant="ghost" size="sm" onClick={onClose} data-testid="lightbox-close">
        {isTerminal ? t("attachment.lightboxCloseTerminal") : t("common:actions.close")}
      </Button>
    </>
  );
};
