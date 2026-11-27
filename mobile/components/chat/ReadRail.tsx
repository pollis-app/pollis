/*
 * The read-up-to avatar (#2140, docs/design/README.md → "Read receipts"). In a one-to-one DM the
 * other person's small avatar sits on a narrow rail along the list's right edge, level with the
 * newest message of yours they have read.
 *
 * It reads as ONE element. Every row of a one-to-one DM holds a rail slot; only the read-up-to
 * row's slot draws the avatar, and when that moves to a newer row the avatar starts where it was
 * (both slots measured in the window) and glides down on `MOTION` timing (`BASE_TIMING`), on the
 * UI thread. It lives inside its row, so it scrolls with the list. Under Reduce Motion it moves
 * without the glide. Nothing shows until something is read (lib/readUpTo.ts), and with receipts
 * off the screen never places it (#892).
 */
import { createContext, useCallback, useContext, useEffect, useRef, type ReactNode } from "react";
import { View } from "react-native";
import Animated, {
  useAnimatedStyle,
  useSharedValue,
  withTiming,
} from "react-native-reanimated";
import { UserAvatar } from "../UserAvatar";
import { BASE_TIMING } from "../../theme/motion";
import { useReduceMotion } from "../../hooks/useReduceMotion";

/** The rail's width; rows of a one-to-one DM keep this much clear at their end edge. */
export const READ_RAIL_WIDTH = 28;
const AVATAR_SIZE = 16;

/** Per open conversation: where each row's slot is, and where the avatar was last drawn. */
interface RailRegistry {
  slots: Map<string, View>;
  shownAt: string | null;
}

const RailContext = createContext<RailRegistry | null>(null);

export function ReadRailProvider({ conversationKey, children }: { conversationKey: string; children: ReactNode }) {
  const registry = useRef<{ key: string; value: RailRegistry } | null>(null);
  if (registry.current?.key !== conversationKey) {
    registry.current = { key: conversationKey, value: { slots: new Map(), shownAt: null } };
  }
  return <RailContext.Provider value={registry.current.value}>{children}</RailContext.Provider>;
}

function bottomInWindow(view: View): Promise<number | null> {
  return new Promise((resolve) => {
    view.measureInWindow((_x, y, _w, h) => {
      resolve(Number.isFinite(y) && h > 0 ? y + h : null);
    });
  });
}

/** The peer the avatar shows: who they are and their picture. */
export interface RailPeer {
  userId: string;
  avatarKey?: string | null;
  label: string;
}

/**
 * One row's place on the rail, laid over the row's end edge. `peer` is set only on the
 * read-up-to row; every other slot is empty and only marks where its row is.
 */
export function ReadRailSlot({ messageId, peer }: { messageId: string; peer: RailPeer | null }) {
  const registry = useContext(RailContext);
  const ref = useRef<View>(null);
  const setRef = useCallback(
    (view: View | null) => {
      const previous = ref.current;
      ref.current = view;
      if (!registry) {
        return;
      }
      if (view) {
        registry.slots.set(messageId, view);
      } else if (previous && registry.slots.get(messageId) === previous) {
        registry.slots.delete(messageId);
      }
    },
    [registry, messageId],
  );
  return (
    <View
      ref={setRef}
      pointerEvents="none"
      importantForAccessibility="no-hide-descendants"
      accessibilityElementsHidden
      style={{ position: "absolute", top: 0, bottom: 0, end: 0, width: READ_RAIL_WIDTH }}
    >
      {peer ? <RailAvatar messageId={messageId} peer={peer} slot={ref} registry={registry} /> : null}
    </View>
  );
}

function RailAvatar({
  messageId,
  peer,
  slot,
  registry,
}: {
  messageId: string;
  peer: RailPeer;
  slot: React.RefObject<View | null>;
  registry: RailRegistry | null;
}) {
  const reduceMotion = useReduceMotion();
  const from = registry?.shownAt ?? null;
  // A glide needs somewhere to come from: a different row whose slot is still laid out.
  const glides = !!registry && from !== null && from !== messageId && registry.slots.has(from);
  const offset = useSharedValue(0);
  const opacity = useSharedValue(glides ? 0 : 1);

  useEffect(() => {
    if (!registry) {
      return;
    }
    registry.shownAt = messageId;
    const fromSlot = glides && from ? registry.slots.get(from) : undefined;
    const toSlot = slot.current;
    if (!fromSlot || !toSlot) {
      // First read, or the old row scrolled out of the window: it simply appears.
      opacity.value = 1;
      return;
    }
    let cancelled = false;
    void Promise.all([bottomInWindow(fromSlot), bottomInWindow(toSlot)]).then(([a, b]) => {
      if (cancelled) {
        return;
      }
      if (a === null || b === null || reduceMotion) {
        opacity.value = 1;
        return;
      }
      // Start level with the old row, then travel to this one. List cells are flipped twice
      // under `inverted`, so a row's own +y is the window's +y.
      offset.value = a - b;
      opacity.value = 1;
      offset.value = withTiming(0, BASE_TIMING);
    });
    return () => {
      cancelled = true;
    };
    // Runs once per placement; `glides`/`from` are read as they were when this row took it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [registry, messageId]);

  const style = useAnimatedStyle(() => ({
    opacity: opacity.value,
    transform: [{ translateY: offset.value }],
  }));

  return (
    <Animated.View
      testID={`read-up-to-${messageId}`}
      style={[
        {
          position: "absolute",
          bottom: 4,
          start: (READ_RAIL_WIDTH - AVATAR_SIZE) / 2,
        },
        style,
      ]}
    >
      <UserAvatar
        userId={peer.userId}
        avatarKey={peer.avatarKey}
        label={peer.label}
        size={AVATAR_SIZE}
        presence={false}
      />
    </Animated.View>
  );
}
