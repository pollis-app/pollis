// The top-end toolbar on a pending attachment card (#1585), desktop's
// ✕ / edit / delete strip on its composer preview. One filled pill of icon
// buttons, so a new control (#1568's sensitive toggle) is one more entry in
// `actions`, not a new layout.
//
// Each button is a 26pt disc and the hit slop takes every target to 44pt.
// It is the theme's own filled control, the composer's attach button in
// small: the raised surface with a text-coloured glyph, one surface up while
// pressed, and the accent fill with the background-coloured glyph while
// `selected` (a toggle that is on). Every colour is a derived token, so it
// follows the accent and background the user picked. No borders and no ring
// in any state: the pill hugs its buttons, so a lone button is just a disc.

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

const BUTTON = 26;
// The glyph size an action should draw at: 16 on a 30pt disc, scaled down.
export const TOOLBAR_GLYPH = 14;
// Brings a 26pt disc to the 44pt target.
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
        gap: 2,
        borderRadius: BUTTON / 2,
        backgroundColor: semantic.raised,
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
                  : "transparent",
            })}
          >
            {action.icon(on ? semantic.onAccent : semantic.text)}
          </Pressable>
        );
      })}
    </View>
  );
}
