import { useEffect, useState } from "react";
import { Text } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { useTranslation } from "react-i18next";
import { Screen, Header } from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import { useResolvePermalink } from "../../hooks/queries";
import { useConversationRoute } from "../../hooks/useConversationRoute";
import { permalinkMissCopy } from "../../lib/permalinks";
import { messageJumpStore } from "../../stores/messageJumpStore";
import { useThemeRerender } from "../../components/theme";

/** `pollis://m/<conversation>/<message>` (#887): a local resolve; every miss shows the same non-oracle copy. */
export default function PermalinkScreen() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const { openConversation } = useOpenConversation();
  const params = useLocalSearchParams<{ permalink?: string | string[] }>();
  const resolvePermalink = useResolvePermalink();
  const routeFor = useConversationRoute();
  const [state, setState] = useState<"checking" | "missing">("checking");

  const segments = Array.isArray(params.permalink)
    ? params.permalink
    : typeof params.permalink === "string"
      ? params.permalink.split("/")
      : [];
  const conversationId = segments[0] ?? null;
  const messageId = segments[1] ?? null;

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (!conversationId || !messageId || segments.length !== 2) {
        setState("missing");
        return;
      }
      const target = await resolvePermalink(conversationId, messageId);
      if (cancelled) {
        return;
      }
      if (!target.found) {
        setState("missing");
        return;
      }
      messageJumpStore.request(conversationId, messageId);
      const route = routeFor(conversationId);
      openConversation(
        { id: conversationId, kind: route.kind, name: route.name ?? undefined },
        { replace: true },
      );
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId, messageId]);

  return (
    <Screen testID="screen-permalink">
      <Header title={t("mobile:permalink.title")} />
      <Text
        testID={state === "missing" ? "permalink-unresolved" : "permalink-checking"}
        accessibilityLiveRegion="polite"
        style={[
          ty.body,
          {
            color: state === "missing" ? semantic.text : semantic.muted,
            paddingHorizontal: 16,
            paddingTop: 16,
          },
        ]}
      >
        {state === "missing"
          ? permalinkMissCopy()
          : t("mobile:permalink.opening")}
      </Text>
    </Screen>
  );
}
