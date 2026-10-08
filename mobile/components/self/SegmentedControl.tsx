import { Pressable, Text, View } from "react-native";
import { semantic, fonts, layout, space } from "../../theme/tokens";

export interface Segment<K extends string> {
  key: K;
  label: string;
  testID?: string;
}

/**
 * A two-or-more way view switch (e.g. QR code / Code): one pill track with a
 * segment per option, each a full 44pt tall so the visible shape is the hit
 * area. The selected segment is filled accentSoft with an accentLine border
 * and accent label, and announced as the selected tab.
 */
export function SegmentedControl<K extends string>({
  segments,
  value,
  onChange,
}: {
  segments: readonly Segment<K>[];
  value: K;
  onChange: (key: K) => void;
}) {
  return (
    <View
      accessibilityRole="tablist"
      style={{
        flexDirection: "row",
        alignSelf: "center",
        padding: 2,
        gap: 2,
        borderRadius: layout.touchMin / 2 + 2,
        backgroundColor: semantic.raised,
      }}
    >
      {segments.map((s) => {
        const selected = s.key === value;
        return (
          <Pressable
            key={s.key}
            testID={s.testID}
            accessibilityRole="tab"
            accessibilityLabel={s.label}
            accessibilityState={{ selected }}
            onPress={() => onChange(s.key)}
            style={{
              minHeight: layout.touchMin,
              minWidth: 96,
              paddingHorizontal: space.xxl,
              justifyContent: "center",
              alignItems: "center",
              borderRadius: layout.touchMin / 2,
              borderWidth: 1,
              borderColor: selected ? semantic.accentLine : "transparent",
              backgroundColor: selected ? semantic.accentSoft : "transparent",
            }}
          >
            <Text
              style={{
                fontFamily: fonts.semibold,
                fontSize: 14,
                color: selected ? semantic.accent : semantic.text,
              }}
            >
              {s.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
