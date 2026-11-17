import React from "react";
import { useTranslation } from "react-i18next";
import { Switch } from "../ui/Switch";
import {
  useLinkPreviewsEnabled,
  useSetLinkPreviewsEnabled,
} from "../../hooks/queries/useLinkPreviews";
import type { OverlayMode } from "../../hooks/queries/usePreferences";

interface LinkPreviewsSectionProps {
  /** Strict network privacy never fetches a page, so the switch reads off and cannot be turned on. */
  overlayMode: OverlayMode;
}

/** "Generate link previews" (#1987): this device's own sends only; received previews always render. */
export const LinkPreviewsSection: React.FC<LinkPreviewsSectionProps> = ({ overlayMode }) => {
  const { t } = useTranslation("settings");
  const enabled = useLinkPreviewsEnabled();
  const setEnabled = useSetLinkPreviewsEnabled();
  const strict = overlayMode === "strict";

  return (
    <section className="flex flex-col gap-4 mb-12" data-testid="pref-link-previews">
      <h2 className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line">
        {t("linkPreviews.heading")}
      </h2>
      <Switch
        id="pref-link-previews-enabled"
        data-testid="pref-link-previews-enabled"
        label={t("linkPreviews.label")}
        description={t("linkPreviews.description")}
        checked={enabled && !strict}
        disabled={strict}
        onChange={(next) => setEnabled.mutate(next)}
      />
      {strict ? (
        <p data-testid="pref-link-previews-strict" className="text-xs font-mono text-muted">
          {t("linkPreviews.strictNote")}
        </p>
      ) : null}
    </section>
  );
};
