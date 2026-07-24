import { type ReactNode } from "react";
import { View, Text } from "react-native";
import { semantic, type as ty, layout } from "../theme/tokens";

// Two-pane master-detail primitives for the regular (iPad) layout — issue #622.
// Only rendered when `useLayoutClass() === "regular"`; the compact tree never
// mounts these, so phone behavior is untouched.

// Right-pane empty state — shown when nothing is selected yet.
export function DetailPlaceholder() {
  return (
    <View
      style={{
        flex: 1,
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
      }}
    >
      <Text
        style={{
          fontFamily: ty.body.fontFamily,
          fontSize: 14,
          color: semantic.mute,
        }}
      >
        Select a conversation
      </Text>
    </View>
  );
}

// Left list column (fixed `listPaneWidth`) + 1px hairline divider + flexible
// right detail column, mirroring desktop's sidebar+content split.
export function TwoPane({
  list,
  detail,
}: {
  list: ReactNode;
  detail: ReactNode;
}) {
  return (
    <View style={{ flexDirection: "row", flex: 1 }}>
      <View style={{ width: layout.listPaneWidth }}>{list}</View>
      <View style={{ width: 1, backgroundColor: semantic.hairSoft }} />
      <View style={{ flex: 1 }}>{detail}</View>
    </View>
  );
}
