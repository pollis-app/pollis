import { StyleSheet, View } from 'react-native';
import { Check } from 'lucide-react-native';

import { Text } from './Text';
import { colors, radius, spacing } from '../theme/tokens';

export function EmptyState() {
  return (
    <View style={styles.root}>
      <View style={styles.mark}>
        <Check size={28} color={colors.tertiary} strokeWidth={1.25} />
      </View>
      <Text weight="semibold" size={32} style={styles.title}>
        You're caught up
      </Text>
      <Text size={15} color={colors.onSurfaceVariant} style={styles.subtitle}>
        Nothing new from your groups. Pull from the top to sign out or wait for
        a nudge.
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    flex: 1,
    alignItems: 'center',
    justifyContent: 'center',
    paddingHorizontal: spacing.xl,
  },
  mark: {
    width: 64,
    height: 64,
    borderRadius: radius.full,
    backgroundColor: colors.tertiaryMuted,
    alignItems: 'center',
    justifyContent: 'center',
    marginBottom: spacing.lg,
  },
  title: {
    letterSpacing: -0.7,
    marginBottom: spacing.sm,
    textAlign: 'center',
  },
  subtitle: {
    lineHeight: 22,
    textAlign: 'center',
    maxWidth: 280,
  },
});
