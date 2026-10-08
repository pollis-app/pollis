import { useEffect, useState } from "react";
import { Text } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header } from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import { useResolvePermalink } from "../../hooks/queries";
import { useConversationRoute } from "../../hooks/useConversationRoute";
import { permalinkMissCopy } from "../../lib/permalinks";

/**
 * Deep-link target for `pollis://m/<conversation_id>/<message_id>` (#887).
 * Resolves locally via `resolve_message_permalink`; on success it replaces
 * itself with the conversation, on a miss it shows desktop's exact
 * non-oracle copy and navigates nowhere (a malformed link, a failed lookup,
 * and a message this device does not hold are indistinguishable).
 */
export default function PermalinkScreen() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
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
      const route = routeFor(conversationId);
      router.replace({
        pathname: "/chat/[id]",
        params: {
          id: conversationId,
          kind: route.kind,
          ...(route.name ? { name: route.name } : {}),
        },
      });
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
