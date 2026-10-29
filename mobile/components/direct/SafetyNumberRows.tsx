import { View } from "react-native";
import { useTranslation } from "react-i18next";
import { Group, ListRow } from "../ui";
import { Icon } from "../icons";
import { semantic, space } from "../../theme/tokens";
import { useNav } from "../pane/paneContext";
import { useThemeRerender } from "../theme";

export interface SafetyPeer {
  user_id: string;
  username?: string | null;
}

// The DM's "Verify safety number" entry (#1563). A safety number belongs to a
// PAIR — your key with theirs — so a 1:1 shows one row and a group DM one row
// per other member ("You & @name"), never one number for the conversation.
// Shared by DM info and the members screen the chat header opens.
export function SafetyNumberRows({ peers }: { peers: SafetyPeer[] }) {
  useThemeRerender();
  const { t } = useTranslation("dms");
  const router = useNav();
  if (peers.length === 0) {
    return null;
  }
  const single = peers.length === 1;
  return (
    <View style={{ paddingHorizontal: space.xxl, paddingTop: space.xl }}>
      <Group title={single ? t("profile.safetyNumber") : t("safety.headingMany")}>
        {peers.map((m) => {
          const name = `@${m.username ?? m.user_id.slice(0, 8)}`;
          return (
            <ListRow
              key={m.user_id}
              testID={`row-verify-safety-${m.user_id}`}
              glyph={<Icon.shield size={20} color={semantic.text} />}
              name={single ? t("safety.verify") : t("safety.title", { name })}
              chevron
              onPress={() =>
                router.push({
                  pathname: "/dm/safety",
                  params: { peerId: m.user_id, name },
                })
              }
            />
          );
        })}
      </Group>
    </View>
  );
}
