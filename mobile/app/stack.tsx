import { useEffect, useRef, useState } from 'react';
import {
  Dimensions,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  StyleSheet,
  TextInput,
  View,
} from 'react-native';
import * as Notifications from 'expo-notifications';
import { Gesture, GestureDetector } from 'react-native-gesture-handler';
import Animated, {
  Extrapolation,
  interpolate,
  runOnJS,
  useAnimatedStyle,
  useSharedValue,
  withSpring,
  withTiming,
} from 'react-native-reanimated';
import { useRouter } from 'expo-router';
import { useSafeAreaInsets } from 'react-native-safe-area-context';
import BottomSheet from '@gorhom/bottom-sheet';
import { Send, X } from 'lucide-react-native';

import { Text } from '../components/Text';
import { MessageCard, FakeMessage } from '../components/MessageCard';
import { ReplySheet } from '../components/ReplySheet';
import { DisconnectSheet } from '../components/DisconnectSheet';
import { EmptyState } from '../components/EmptyState';
import { SignOutDrawer, DRAWER_HEIGHT } from '../components/SignOutDrawer';
import { colors, fonts, radius, spacing } from '../theme/tokens';

const { width: SCREEN_W } = Dimensions.get('window');
const DISMISS_THRESHOLD = SCREEN_W * 0.3;
const REPLY_THRESHOLD = SCREEN_W * 0.3;
const OVERSCROLL_THRESHOLD = DRAWER_HEIGHT * 0.65;
const FOCUS_LIFT_THRESHOLD = 90;
const FOCUS_LIFT_MAX = 140;

// Three hard-coded demo cards.
const FAKE_MESSAGES: FakeMessage[] = [
  {
    id: '1',
    author: 'Amelia Ruiz',
    handle: '@amelia',
    group: 'Voyager',
    channel: 'ops',
    timeAgo: '2m',
    body: 'Thruster calibration is drifting again — can you take a look before the burn?',
    avatarBlurHash: 'LKO2?U%2Tw=w]~RBVZRi};RPxuwH',
  },
  {
    id: '2',
    author: 'Noor Hassan',
    handle: '@noor',
    group: 'Voyager',
    channel: 'incidents',
    timeAgo: '8m',
    body: 'Heads up: the relay in sector 4 is flapping. Paging on-call in five if it doesn\'t settle.',
    avatarBlurHash: 'LEHV6nWB2yk8pyo0adR*.7kCMdnj',
  },
  {
    id: '3',
    author: 'Jun Park',
    handle: '@jun',
    group: 'Crew',
    channel: 'random',
    timeAgo: '23m',
    body: 'Movie night moved to 8pm. Bringing the weird Korean popcorn this time.',
    avatarBlurHash: 'L6PZfSi_.AyE_3t7t7R**0o#DgR4',
  },
];

export default function CardStackScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const replySheetRef = useRef<BottomSheet>(null);
  const disconnectSheetRef = useRef<BottomSheet>(null);

  const [cards, setCards] = useState<FakeMessage[]>(FAKE_MESSAGES);
  const [activeReply, setActiveReply] = useState<FakeMessage | null>(null);
  const [focusedCardId, setFocusedCardId] = useState<string | null>(null);
  const [inlineDraft, setInlineDraft] = useState('');

  // Swipe state for the top card.
  const translateX = useSharedValue(0);
  const translateY = useSharedValue(0);
  // Vertical pull state for the sign-out drawer reveal.
  const pullY = useSharedValue(0);
  // 0 when drawer is closed, 1 when pinned open.
  const drawerPinned = useSharedValue(0);

  useEffect(() => {
    // Ask for notification permission once the user reaches the real screen.
    Notifications.requestPermissionsAsync().catch(() => {});
  }, []);

  const openReply = (card: FakeMessage) => {
    setActiveReply(card);
    replySheetRef.current?.snapToIndex(0);
  };

  const dismissTop = () => {
    setCards((prev) => prev.slice(1));
  };

  const handleReplyClosed = () => {
    setActiveReply(null);
  };

  const handleReplySheetSend = () => {
    replySheetRef.current?.close();
    dismissTop();
  };

  const enterFocus = (cardId: string) => {
    setFocusedCardId(cardId);
  };

  const exitFocus = () => {
    setFocusedCardId(null);
    setInlineDraft('');
  };

  const handleInlineSend = () => {
    setFocusedCardId(null);
    setInlineDraft('');
    dismissTop();
  };

  const topCard = cards[0];
  const nextCard = cards[1];
  const isFocused = focusedCardId !== null && topCard?.id === focusedCardId;

  const pan = Gesture.Pan()
    .activeOffsetX([-8, 8])
    .activeOffsetY([-8, 8])
    .enabled(!isFocused)
    .onUpdate((e) => {
      const horizDominant = Math.abs(e.translationX) > Math.abs(e.translationY);
      const isDrawerOpen = drawerPinned.value === 1;

      if (horizDominant) {
        // Horizontal swipes only count when drawer is closed.
        if (isDrawerOpen) {
          return;
        }
        translateX.value = e.translationX;
        translateY.value = e.translationY * 0.2;
        pullY.value = 0;
        return;
      }

      // Vertical gesture: either drives the drawer reveal (downward) or
      // previews a focus-lift (upward while drawer closed).
      const basePull = isDrawerOpen ? DRAWER_HEIGHT : 0;
      const next = basePull + e.translationY;
      pullY.value = Math.min(Math.max(next, 0), DRAWER_HEIGHT * 1.2);

      if (!isDrawerOpen && e.translationY < 0) {
        translateY.value = Math.max(e.translationY, -FOCUS_LIFT_MAX);
        translateX.value = 0;
      } else {
        translateX.value = 0;
        translateY.value = 0;
      }
    })
    .onEnd((e) => {
      const horizDominant = Math.abs(e.translationX) > Math.abs(e.translationY);
      const isDrawerOpen = drawerPinned.value === 1;

      if (horizDominant && !isDrawerOpen) {
        if (e.translationX < -DISMISS_THRESHOLD) {
          translateX.value = withTiming(-SCREEN_W * 1.4, { duration: 260 }, () => {
            runOnJS(dismissTop)();
            translateX.value = 0;
            translateY.value = 0;
          });
          return;
        }
        if (e.translationX > REPLY_THRESHOLD) {
          translateX.value = withSpring(0, { damping: 18, stiffness: 180 });
          translateY.value = withSpring(0, { damping: 18, stiffness: 180 });
          if (topCard) {
            runOnJS(openReply)(topCard);
          }
          return;
        }
        translateX.value = withSpring(0, { damping: 18, stiffness: 180 });
        translateY.value = withSpring(0, { damping: 18, stiffness: 180 });
        return;
      }

      // Vertical
      if (!isDrawerOpen && e.translationY < -FOCUS_LIFT_THRESHOLD && topCard) {
        // Upward swipe past threshold → enter focus mode.
        translateY.value = withSpring(0, { damping: 20, stiffness: 200 });
        pullY.value = withSpring(0, { damping: 20, stiffness: 200 });
        runOnJS(enterFocus)(topCard.id);
        return;
      }

      if (pullY.value > OVERSCROLL_THRESHOLD) {
        pullY.value = withSpring(DRAWER_HEIGHT, {
          damping: 18,
          stiffness: 160,
        });
        drawerPinned.value = 1;
      } else {
        pullY.value = withSpring(0, { damping: 20, stiffness: 200 });
        drawerPinned.value = 0;
      }
      translateY.value = withSpring(0, { damping: 20, stiffness: 200 });
    });

  const doubleTap = Gesture.Tap()
    .numberOfTaps(2)
    .onEnd(() => {
      if (topCard) {
        runOnJS(enterFocus)(topCard.id);
      }
    });

  const cardGesture = Gesture.Race(doubleTap, pan);

  const topCardStyle = useAnimatedStyle(() => {
    const rotate = interpolate(
      translateX.value,
      [-SCREEN_W, 0, SCREEN_W],
      [-8, 0, 8],
      Extrapolation.CLAMP,
    );
    return {
      transform: [
        { translateX: translateX.value },
        { translateY: translateY.value + pullY.value },
        { rotate: `${rotate}deg` },
      ],
    };
  });

  const nextCardStyle = useAnimatedStyle(() => {
    const progress = Math.min(Math.abs(translateX.value) / DISMISS_THRESHOLD, 1);
    const scale = interpolate(progress, [0, 1], [0.94, 0.98]);
    const opacity = interpolate(progress, [0, 1], [0.6, 0.9]);
    return {
      transform: [{ scale }, { translateY: pullY.value * 0.4 }],
      opacity,
    };
  });

  const leftHintStyle = useAnimatedStyle(() => {
    return {
      opacity: interpolate(
        translateX.value,
        [-DISMISS_THRESHOLD, 0],
        [1, 0],
        Extrapolation.CLAMP,
      ),
    };
  });

  const rightHintStyle = useAnimatedStyle(() => {
    return {
      opacity: interpolate(
        translateX.value,
        [0, REPLY_THRESHOLD],
        [0, 1],
        Extrapolation.CLAMP,
      ),
    };
  });

  const handleDisconnectRequest = () => {
    disconnectSheetRef.current?.snapToIndex(0);
  };

  const handleDisconnectConfirm = () => {
    router.replace('/');
  };

  if (isFocused && topCard) {
    return (
      <KeyboardAvoidingView
        style={styles.root}
        behavior={Platform.OS === 'ios' ? 'padding' : undefined}
      >
        <View style={[styles.focusCard, { paddingTop: insets.top }]}>
          <MessageCard message={topCard} variant="flush" />
        </View>
        <View style={styles.composeArea}>
          <TextInput
            value={inlineDraft}
            onChangeText={setInlineDraft}
            placeholder="Reply…"
            placeholderTextColor={colors.onSurfaceVariant}
            style={styles.composeInput}
            autoFocus
            multiline
            textAlignVertical="top"
          />
        </View>
        <View
          style={[
            styles.composeActions,
            { paddingBottom: insets.bottom + spacing.md },
          ]}
        >
          <Pressable
            onPress={exitFocus}
            style={({ pressed }) => [
              styles.inlineClose,
              pressed && styles.inlinePressed,
            ]}
            hitSlop={8}
          >
            <X size={20} color={colors.onSurfaceVariant} strokeWidth={1.5} />
          </Pressable>
          <Pressable
            onPress={handleInlineSend}
            disabled={inlineDraft.trim().length === 0}
            style={({ pressed }) => [
              styles.inlineSend,
              inlineDraft.trim().length === 0 && styles.inlineSendDisabled,
              pressed && styles.inlinePressed,
            ]}
            hitSlop={8}
          >
            <Send size={20} color={colors.surfaceLowest} strokeWidth={1.5} />
          </Pressable>
        </View>
      </KeyboardAvoidingView>
    );
  }

  return (
    <View style={[styles.root, { paddingTop: insets.top }]}>
      <SignOutDrawer pullY={pullY} onSignOut={handleDisconnectRequest} />

      <GestureDetector gesture={cardGesture}>
        <View style={styles.cardArea}>
          {topCard ? (
            <View style={StyleSheet.absoluteFill}>
              {nextCard ? (
                <Animated.View style={[styles.cardWrap, nextCardStyle]}>
                  <MessageCard message={nextCard} />
                </Animated.View>
              ) : null}

              <Animated.View style={[styles.cardWrap, topCardStyle]}>
                <MessageCard message={topCard} />

                <Animated.View
                  style={[styles.hint, styles.hintLeft, leftHintStyle]}
                  pointerEvents="none"
                >
                  <Text weight="medium" size={12} color={colors.onSurfaceVariant}>
                    DISMISS
                  </Text>
                </Animated.View>
                <Animated.View
                  style={[styles.hint, styles.hintRight, rightHintStyle]}
                  pointerEvents="none"
                >
                  <Text weight="medium" size={12} color={colors.tertiary}>
                    REPLY
                  </Text>
                </Animated.View>
              </Animated.View>
            </View>
          ) : (
            <EmptyState />
          )}
        </View>
      </GestureDetector>

      <View
        style={[styles.footer, { paddingBottom: insets.bottom + spacing.md }]}
      >
        <Text size={12} color={colors.onSurfaceVariant} weight="medium">
          {topCard
            ? `${cards.length} waiting · swipe · double-tap to focus`
            : 'Pull down to disconnect'}
        </Text>
      </View>

      <ReplySheet
        ref={replySheetRef}
        onSend={handleReplySheetSend}
        onClose={handleReplyClosed}
      />
      <DisconnectSheet
        ref={disconnectSheetRef}
        onConfirm={handleDisconnectConfirm}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    flex: 1,
    backgroundColor: colors.background,
  },
  cardArea: {
    flex: 1,
    paddingHorizontal: spacing.lg,
    paddingVertical: spacing.lg,
  },
  cardWrap: {
    ...StyleSheet.absoluteFillObject,
    margin: spacing.lg,
  },
  hint: {
    position: 'absolute',
    top: spacing.lg,
    paddingHorizontal: spacing.md,
    paddingVertical: 6,
    borderRadius: 999,
    backgroundColor: colors.surfaceLow,
  },
  hintLeft: {
    left: spacing.lg,
  },
  hintRight: {
    right: spacing.lg,
    backgroundColor: colors.tertiaryMuted,
  },
  footer: {
    alignItems: 'center',
    paddingTop: spacing.sm,
  },
  focusCard: {
    backgroundColor: colors.surfaceLowest,
  },
  composeArea: {
    flex: 1,
    backgroundColor: colors.background,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.md,
  },
  composeInput: {
    flex: 1,
    fontFamily: fonts.regular,
    fontSize: 18,
    lineHeight: 26,
    color: colors.onSurface,
    paddingVertical: spacing.sm,
  },
  composeActions: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    alignItems: 'center',
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.sm,
    backgroundColor: colors.background,
  },
  inlineClose: {
    width: 48,
    height: 48,
    borderRadius: radius.full,
    backgroundColor: colors.surfaceLow,
    alignItems: 'center',
    justifyContent: 'center',
  },
  inlineSend: {
    width: 48,
    height: 48,
    borderRadius: radius.full,
    backgroundColor: colors.tertiary,
    alignItems: 'center',
    justifyContent: 'center',
  },
  inlineSendDisabled: {
    opacity: 0.4,
  },
  inlinePressed: {
    opacity: 0.8,
  },
});
