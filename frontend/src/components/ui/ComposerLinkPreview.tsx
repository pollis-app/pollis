import React from "react";
import { parseLink } from "../../utils/links";
import type { LinkPreviewState } from "../../utils/linkPreview";
import { LinkPreviewCard } from "../Message/LinkPreviewCard";

interface ComposerLinkPreviewProps {
  state: LinkPreviewState;
  onRemove: () => void;
}

/** The draft's link preview above a composer or the edit bar (#1987): loading, then the card, with ×. */
export const ComposerLinkPreview: React.FC<ComposerLinkPreviewProps> = ({ state, onRemove }) => {
  if (state.status === "none") {
    return null;
  }
  const host = parseLink(state.url)?.host ?? "";
  const image = state.status === "ready" ? state.draft.image : null;
  return (
    <div className="px-2 pt-2" data-testid="composer-link-preview">
      <LinkPreviewCard
        host={host}
        loading={state.status === "loading"}
        title={state.status === "ready" ? state.draft.title : ""}
        description={state.status === "ready" ? state.draft.description : null}
        thumb={
          image ? (
            <img src={image.dataUrl} alt="" draggable={false} className="h-full w-full object-cover" />
          ) : undefined
        }
        onRemove={onRemove}
      />
    </div>
  );
};
