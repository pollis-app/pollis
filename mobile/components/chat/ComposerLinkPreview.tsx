// The draft's link preview above the composer or the edit bar (#1987): loading, then the card, with ×.

import { View } from "react-native";
import { Image } from "expo-image";
import { LinkPreviewCard } from "./LinkPreviewCard";
import { parseLink } from "../../lib/links";
import type { LinkPreviewState } from "../../lib/linkPreview";
import { space } from "../../theme/tokens";

export function ComposerLinkPreview({
  state,
  onRemove,
}: {
  state: LinkPreviewState;
  onRemove: () => void;
}) {
  if (state.status === "none") {
    return null;
  }
  const image = state.status === "ready" ? state.draft.image : null;
  return (
    <View testID="composer-link-preview" style={{ paddingHorizontal: space.lg, paddingTop: space.sm }}>
      <LinkPreviewCard
        host={parseLink(state.url)?.host ?? ""}
        loading={state.status === "loading"}
        title={state.status === "ready" ? state.draft.title : ""}
        description={state.status === "ready" ? state.draft.description : null}
        thumb={
          image ? (
            <Image
              source={{ uri: image.dataUrl }}
              contentFit="cover"
              // Memory only, never expo-image's disk cache.
              cachePolicy="memory"
              style={{ width: "100%", height: "100%" }}
              accessible={false}
            />
          ) : undefined
        }
        onRemove={onRemove}
      />
    </View>
  );
}
