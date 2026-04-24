import { Pressable, StyleSheet, View } from 'react-native';
import Animated, {
  SharedValue,
  interpolate,
  useAnimatedStyle,
} from 'react-native-reanimated';
import { LogOut, Monitor } from 'lucide-react-native';

import { Text } from './Text';
import { colors, radius, spacing } from '../theme/tokens';

// Reveal threshold — pull down past this to expose the sign-out action.
export const DRAWER_HEIGHT = 220;

interface Props {
  pullY: SharedValue<number>;
  onSignOut: () => void;
}

// Static stub identity — would come from secure storage in the real app.
const FAKE_IDENTITY = {
  name: 'Dan Krall',
  handle: '@dan',
  deviceLabel: 'Pixel 9a · paired to Framework 16',
};

export function SignOutDrawer({ pullY, onSignOut }: Props) {
  const contentStyle = useAnimatedStyle(() => {
    const progress = Math.min(Math.max(pullY.value / DRAWER_HEIGHT, 0), 1);
    return {
      opacity: interpolate(progress, [0, 0.4, 1], [0, 0.5, 1]),
      transform: [
        {
          translateY: interpolate(progress, [0, 1], [-12, 0]),
        },
      ],
    };
  });

  return (
    <View style={styles.root} pointerEvents="box-none">
      <Animated.View style={[styles.content, contentStyle]}>
        <View style={styles.identityRow}>
          <View style={styles.avatarPlaceholder}>
            <Text weight="semibold" size={18} color={colors.tertiary}>
              DK
            </Text>
          </View>
          <View style={styles.identityText}>
            <Text weight="semibold" size={16}>
              {FAKE_IDENTITY.name}
            </Text>
            <Text size={13} color={colors.onSurfaceVariant}>
              {FAKE_IDENTITY.handle}
            </Text>
          </View>
        </View>

        <View style={styles.deviceRow}>
          <Monitor size={14} color={colors.onSurfaceVariant} strokeWidth={1.5} />
          <Text size={12} color={colors.onSurfaceVariant}>
            {FAKE_IDENTITY.deviceLabel}
          </Text>
        </View>

        <Pressable
          onPress={onSignOut}
          style={({ pressed }) => [
            styles.signOutButton,
            pressed && styles.signOutButtonPressed,
          ]}
        >
          <LogOut size={16} color={colors.onSurface} strokeWidth={1.5} />
          <Text weight="medium" size={14} color={colors.onSurface}>
            Disconnect
          </Text>
        </Pressable>
      </Animated.View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    position: 'absolute',
    top: 0,
    left: 0,
    right: 0,
    height: DRAWER_HEIGHT,
    paddingHorizontal: spacing.lg,
    justifyContent: 'flex-end',
    paddingBottom: spacing.md,
  },
  content: {
    gap: spacing.md,
  },
  identityRow: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: spacing.md,
  },
  avatarPlaceholder: {
    width: 44,
    height: 44,
    borderRadius: radius.full,
    backgroundColor: colors.tertiaryMuted,
    alignItems: 'center',
    justifyContent: 'center',
  },
  identityText: {
    flex: 1,
  },
  deviceRow: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: spacing.xs,
  },
  signOutButton: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'center',
    gap: spacing.sm,
    height: 48,
    borderRadius: radius.full,
    backgroundColor: colors.surfaceHighest,
    alignSelf: 'flex-start',
    paddingHorizontal: spacing.lg,
  },
  signOutButtonPressed: {
    opacity: 0.75,
  },
});
