import { StyleSheet, View } from 'react-native';
import { Image } from 'expo-image';

import { Text } from './Text';
import { colors, radius, spacing } from '../theme/tokens';

export interface FakeMessage {
  id: string;
  author: string;
  handle: string;
  group: string;
  channel: string;
  timeAgo: string;
  body: string;
  avatarBlurHash: string;
}

interface Props {
  message: FakeMessage;
  variant?: 'card' | 'flush';
}

export function MessageCard({ message, variant = 'card' }: Props) {
  return (
    <View style={variant === 'flush' ? styles.flush : styles.card}>
      <View style={styles.headerRow}>
        <Image
          source={{ blurhash: message.avatarBlurHash }}
          placeholder={{ blurhash: message.avatarBlurHash }}
          style={styles.avatar}
          contentFit="cover"
        />
        <View style={styles.headerText}>
          <Text weight="semibold" size={16}>
            {message.author}
          </Text>
          <Text size={13} color={colors.onSurfaceVariant}>
            {message.handle} · {message.timeAgo}
          </Text>
        </View>
      </View>

      <View style={styles.context}>
        <Text
          size={11}
          weight="medium"
          color={colors.onSurfaceVariant}
          style={styles.contextLabel}
        >
          {message.group.toUpperCase()} / {message.channel.toUpperCase()}
        </Text>
      </View>

      <Text size={22} weight="regular" style={styles.body}>
        {message.body}
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  card: {
    flex: 1,
    backgroundColor: colors.surfaceLowest,
    borderRadius: radius.xl,
    padding: spacing.lg,
    paddingTop: spacing.xl,
    shadowColor: colors.onSurface,
    shadowOffset: { width: 0, height: 20 },
    shadowOpacity: 0.06,
    shadowRadius: 40,
    elevation: 6,
  },
  flush: {
    backgroundColor: colors.surfaceLowest,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.xl,
    paddingBottom: spacing.lg,
  },
  headerRow: {
    flexDirection: 'row',
    alignItems: 'center',
    marginBottom: spacing.lg,
  },
  avatar: {
    width: 44,
    height: 44,
    borderRadius: radius.full,
    backgroundColor: colors.surfaceLow,
    marginRight: spacing.md,
  },
  headerText: {
    flex: 1,
  },
  context: {
    marginBottom: spacing.md,
  },
  contextLabel: {
    letterSpacing: 1.5,
  },
  body: {
    lineHeight: 32,
    letterSpacing: -0.3,
  },
});
