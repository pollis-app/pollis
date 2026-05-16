import { View, Pressable, Text } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { palette, semantic, fonts } from "../theme/tokens";
import { useTheme } from "./theme";
import { Icon } from "./icons";

const TABS: {
  name: string;
  label: string;
  glyph: (c: string) => React.ReactNode;
}[] = [
  {
    name: "groups",
    label: "Groups",
    glyph: (c) => <Icon.diamond size={16} color={c} />,
  },
  { name: "direct", label: "Direct", glyph: (c) => <Icon.at size={16} color={c} /> },
  {
    name: "search",
    label: "Search",
    glyph: (c) => <Icon.search size={16} color={c} />,
  },
  { name: "self", label: "Self", glyph: (c) => <Icon.user size={16} color={c} /> },
];

export function TabBar({ state, navigation }: any) {
  useTheme();
  const insets = useSafeAreaInsets();
  return (
    <View
      style={{
        flexDirection: "row",
        height: 70 + insets.bottom,
        paddingBottom: insets.bottom,
        borderTopWidth: 1,
        borderTopColor: semantic.hair,
        backgroundColor: palette.bg,
      }}
    >
      {TABS.map((tab, i) => {
        const focused = state.index === i;
        const color = focused ? semantic.accent : semantic.mute;
        return (
          <Pressable
            key={tab.name}
            onPress={() => navigation.navigate(tab.name)}
            style={{
              flex: 1,
              alignItems: "center",
              justifyContent: "center",
              gap: 4,
            }}
          >
            {focused && (
              <View
                style={{
                  position: "absolute",
                  top: 0,
                  width: 22,
                  height: 2,
                  backgroundColor: semantic.accent,
                }}
              />
            )}
            <View
              style={{
                width: 22,
                height: 22,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              {tab.glyph(color)}
            </View>
            <Text
              style={{
                fontFamily: fonts.sora500,
                fontSize: 10,
                letterSpacing: 1.4,
                textTransform: "uppercase",
                color,
              }}
            >
              {tab.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
