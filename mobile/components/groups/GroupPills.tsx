import { Pressable, ScrollView, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Dot, IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic, fonts, space, layout } from "../../theme/tokens";
import { useTheme } from "../theme";

export interface GroupPill {
  id: string;
  name: string;
  unread: boolean;
}

// The horizontal strip of group-name pills at the top of the Groups tab: 32pt
// pills in 44pt targets, no borders. Selected = accent fill + dark (onAccent)
// text; the rest are the inverse, a dark fill + accent text. Unread = a dot
// before the name, plus a bolder label, so it is not told by colour alone.
// Ends with "+" (create or find a group) as an accent circle with a dark
// glyph. Tapping a pill selects that group in place.
export function GroupPills({
  groups,
  selectedId,
  onSelect,
  onAdd,
}: {
  groups: GroupPill[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onAdd: () => void;
}) {
  const { t } = useTranslation("mobile");
  // Re-render on a live accent change; the colours below are getters.
  useTheme();
  return (
    <View accessibilityLabel={t("groups.stripLabel")} style={{ flexShrink: 0 }}>
      <ScrollView
        horizontal
        showsHorizontalScrollIndicator={false}
        contentContainerStyle={{
          alignItems: "center",
          gap: space.xs,
          paddingStart: space.lg,
          paddingEnd: space.sm,
          paddingBottom: 4,
        }}
      >
        {groups.map((g) => {
          const selected = g.id === selectedId;
          const label =
            selected && g.unread
              ? t("groups.pillSelectedUnread", { name: g.name })
              : selected
                ? t("groups.pillSelected", { name: g.name })
                : g.unread
                  ? t("groups.pillUnread", { name: g.name })
                  : g.name;
          const fg = selected ? semantic.onAccent : semantic.accent;
          return (
            <Pressable
              key={g.id}
              testID={`row-group-${g.id}`}
              onPress={() => onSelect(g.id)}
              accessibilityRole="button"
              accessibilityLabel={label}
              accessibilityState={{ selected }}
              style={{ minHeight: layout.touchMin, justifyContent: "center" }}
            >
              {({ pressed }) => (
                <View
                  style={{
                    minHeight: 32,
                    flexDirection: "row",
                    alignItems: "center",
                    gap: space.xs,
                    paddingHorizontal: space.lg,
                    borderRadius: 16,
                    backgroundColor: selected
                      ? semantic.accent
                      : pressed
                        ? semantic.high
                        : semantic.raised,
                  }}
                >
                  {g.unread ? <Dot size={7} color={fg} /> : null}
                  <Text
                    numberOfLines={1}
                    style={{
                      fontFamily: selected || g.unread ? fonts.semibold : fonts.medium,
                      fontSize: 14,
                      color: fg,
                    }}
                  >
                    {g.name}
                  </Text>
                </View>
              )}
            </Pressable>
          );
        })}
        <IconButton
          testID="btn-add-group"
          accessibilityLabel={t("groups.addGroup")}
          onPress={onAdd}
          icon={
            <View
              style={{
                width: 32,
                height: 32,
                borderRadius: 16,
                backgroundColor: semantic.accent,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <Icon.plus size={18} color={semantic.onAccent} />
            </View>
          }
        />
      </ScrollView>
    </View>
  );
}
