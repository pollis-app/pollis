import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../hooks/queries/usePreferences";
import { LoadingSpinner } from "../ui/LoaderSpinner";

/** The lightbox's download button text for each step of a save (#1965): Terminal's bracketed
 *  words, Modern's plain ones. Shared by `AttachmentDisplay` and `MediaSpotlight`. */
export const LightboxDownloadLabel: React.FC<{ status: "idle" | "downloading" | "done" }> = ({ status }) => {
  const { t } = useTranslation("chat");
  const isTerminal = useSkin() === "terminal";
  if (status === "downloading") {
    return isTerminal ? (
      <>[ {t("attachment.lightboxFetch")} <LoadingSpinner size="sm" /> ]</>
    ) : (
      <>{t("attachment.lightboxDownload")} <LoadingSpinner size="sm" /></>
    );
  }
  if (status === "done") {
    return <>{t(isTerminal ? "attachment.lightboxDoneTerminal" : "attachment.lightboxDone")}</>;
  }
  return <>{t(isTerminal ? "attachment.lightboxDownloadTerminal" : "attachment.lightboxDownload")}</>;
};
