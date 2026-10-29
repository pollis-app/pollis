// The top-end toolbar on a pending attachment card (#1585), desktop's
// ✕ / edit / delete strip on its composer preview. One filled pill of icon
// buttons, so a new control (#1568's sensitive toggle) is one more entry in
// `actions`, not a new layout.
//
// Each button is a 30pt disc inside the pill; the hit slop takes every
// target to 44pt. No borders in any state: the pill is the background
// surface over the media, a `selected` button (a toggle that is on) the
// accent fill with the background-coloured glyph.

import type { ReactNode } from "react";
import { Pressable, View } from "react-native";
import { semantic } from "../../theme/tokens";

export interface AttachmentToolbarAction {
  key: string;
  icon: (color: string) => ReactNode;
  accessibilityLabel: string;
  testID: string;
  onPress: () => void;
  // A toggle (the sensitive eye): reports its state to screen readers and
  // draws the accent fill while on. Leave undefined for a plain action.
  selected?: boolean;
}

const BUTTON = 30;
const PAD = 3;
// Brings a 30pt disc to the 44pt target.
const SLOP = (44 - BUTTON) / 2;

export function AttachmentToolbar({
  actions,
  testID,
}: {
  actions: AttachmentToolbarAction[];
  testID?: string;
}) {
  return (
    <View
      testID={testID}
      style={{
        position: "absolute",
        top: 6,
        end: 6,
        flexDirection: "row",
        gap: 4,
        padding: PAD,
        borderRadius: (BUTTON + PAD * 2) / 2,
        backgroundColor: semantic.bg,
      }}
    >
      {actions.map((action) => {
        const on = action.selected === true;
        return (
          <Pressable
            key={action.key}
            testID={action.testID}
            accessibilityRole={action.selected === undefined ? "button" : "togglebutton"}
            accessibilityLabel={action.accessibilityLabel}
            accessibilityState={action.selected === undefined ? undefined : { checked: on }}
            onPress={action.onPress}
            hitSlop={SLOP}
            style={({ pressed }) => ({
              width: BUTTON,
              height: BUTTON,
              borderRadius: BUTTON / 2,
              alignItems: "center",
              justifyContent: "center",
              backgroundColor: on
                ? semantic.accent
                : pressed
                  ? semantic.high
                  : semantic.raised,
            })}
          >
            {action.icon(on ? semantic.onAccent : semantic.text)}
          </Pressable>
        );
      })}
    </View>
  );
}
