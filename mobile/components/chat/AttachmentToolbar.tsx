// The top-end toolbar on a pending attachment card (#1585), desktop's
// ✕ / edit / delete strip on its composer preview, and the hide eye on a
// revealed sensitive attachment (#1568).
//
// ONE capsule on the raised surface holding every glyph — never a row of
// separate discs, which the owner rejected ("two rounded buttons next to each
// other looks bad"). No per-button fill, no divider, no border, no ring. A
// glyph is text-coloured; a toggle that is on (the sensitive eye, marked)
// says so with its glyph alone, drawn in the accent. Pressing a button steps
// up the surface of that button's own area only, clipped to the capsule. A
// lone action is the same capsule with one glyph: a 26pt circle.
//
// The capsule is 26pt tall. Every button reaches a 44pt target through hit
// slop, without growing what is drawn: a lone button slops on every side;
// in a row, each button is wide enough that its outer slop makes 44 across,
// and the inner edges, where two buttons meet, slop nothing, so the targets
// never overlap.

import type { ReactNode } from "react";
import { I18nManager, Pressable, View } from "react-native";
import { semantic } from "../../theme/tokens";

export interface AttachmentToolbarAction {
  key: string;
  icon: (color: string) => ReactNode;
  accessibilityLabel: string;
  testID: string;
  onPress: () => void;
  // A toggle (the sensitive eye): reports its state to screen readers and
  // draws its glyph in the accent while on. Leave undefined for a plain
  // action.
  selected?: boolean;
}

const BUTTON = 26;
// The glyph size an action should draw at: 16 on a 30pt disc, scaled down.
export const TOOLBAR_GLYPH = 14;
// Brings a 26pt button to the 44pt target, top and bottom (and all round for
// a lone button).
const SLOP = (44 - BUTTON) / 2;
// A button that shares the capsule: its own width plus its one outer slop
// makes 44 across.
const SHARED_WIDTH = 34;
const SHARED_OUTER_SLOP = 44 - SHARED_WIDTH;

// Hit slop takes physical edges only, so the start and end edges are mapped
// for the layout direction (Yoga mirrors the row under RTL).
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
