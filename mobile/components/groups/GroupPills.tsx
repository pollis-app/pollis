import { ScrollView, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Chip, Dot, IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic, space } from "../../theme/tokens";

export interface GroupPill {
  id: string;
  name: string;
  unread: boolean;
}

// The horizontal strip of group-name pills at the top of the Groups tab
// (Main.dc.html): 32pt pills in 44pt targets. Selected = accent tint + accent
// border + accent text; unread = a dot before the name; read = dim. Ends with
// "+" (create or find a group). Tapping a pill selects that group in place.
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
  return (
    <View accessibilityLabel={t("groups.stripLabel")} style={{ flexShrink: 0 }}>
      <ScrollView
        horizontal
        showsHorizontalScrollIndicator={false}
        contentContainerStyle={{
          alignItems: "center",
          gap: 2,
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
          return (
            <Chip
              key={g.id}
              testID={`row-group-${g.id}`}
              selected={selected}
              variant={g.unread ? "default" : "subtle"}
              leading={g.unread ? <Dot size={7} color={selected ? semantic.accent : undefined} /> : undefined}
              accessibilityLabel={label}
              onPress={() => onSelect(g.id)}
            >
              {g.name}
            </Chip>
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
                borderWidth: 1,
                borderColor: semantic.edge,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <Icon.plus size={16} color={semantic.text} />
            </View>
          }
        />
      </ScrollView>
    </View>
  );
}
