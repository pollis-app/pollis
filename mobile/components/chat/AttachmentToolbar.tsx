// The top-end toolbar on attachment cards (#1585, #1568): ONE capsule holding every glyph, never separate discs. 44pt targets via hit slop that never overlaps.

import type { ReactNode } from "react";
import { I18nManager, Pressable, View } from "react-native";
import { semantic } from "../../theme/tokens";

export interface AttachmentToolbarAction {
  key: string;
  icon: (color: string) => ReactNode;
  accessibilityLabel: string;
  testID: string;
  onPress: () => void;
  // A toggle: reports its state and draws its glyph in the accent while on.
  selected?: boolean;
}

const BUTTON = 26;
export const TOOLBAR_GLYPH = 14;
const SLOP = (44 - BUTTON) / 2;
const SHARED_WIDTH = 34;
const SHARED_OUTER_SLOP = 44 - SHARED_WIDTH;

// Hit slop takes physical edges, so start/end are mapped for RTL.
function sharedSlop(first: boolean, last: boolean) {
  const start = first ? SHARED_OUTER_SLOP : 0;
  const end = last ? SHARED_OUTER_SLOP : 0;
  return I18nManager.isRTL
    ? { top: SLOP, bottom: SLOP, left: end, right: start }
    : { top: SLOP, bottom: SLOP, left: start, right: end };
}

export function AttachmentToolbar({
  actions,
  testID,
}: {
  actions: AttachmentToolbarAction[];
  testID?: string;
}) {
  const lone = actions.length === 1;
  return (
    <View
      testID={testID}
      style={{
        position: "absolute",
        top: 6,
        end: 6,
        flexDirection: "row",
        height: BUTTON,
        borderRadius: BUTTON / 2,
        overflow: "hidden",
        backgroundColor: semantic.raised,
      }}
    >
      {actions.map((action, i) => {
        const on = action.selected === true;
        const first = i === 0;
        const last = i === actions.length - 1;
        return (
          <Pressable
            key={action.key}
            testID={action.testID}
            accessibilityRole={action.selected === undefined ? "button" : "togglebutton"}
            accessibilityLabel={action.accessibilityLabel}
            accessibilityState={action.selected === undefined ? undefined : { checked: on }}
            onPress={action.onPress}
            hitSlop={
              lone
                ? SLOP
                : sharedSlop(first, last)
            }
            style={({ pressed }) => ({
              width: lone ? BUTTON : SHARED_WIDTH,
              height: BUTTON,
              alignItems: "center",
              justifyContent: "center",
              backgroundColor: pressed ? semantic.high : "transparent",
            })}
          >
            {action.icon(on ? semantic.accent : semantic.text)}
          </Pressable>
        );
      })}
    </View>
  );
}
