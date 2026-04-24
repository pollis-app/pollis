import { forwardRef, useCallback, useMemo } from 'react';
import { Pressable, StyleSheet, View } from 'react-native';
import BottomSheet, { BottomSheetBackdrop } from '@gorhom/bottom-sheet';
import { AlertTriangle } from 'lucide-react-native';

import { Text } from './Text';
import { colors, radius, spacing } from '../theme/tokens';

interface Props {
  onConfirm?: () => void;
  onCancel?: () => void;
}

export const DisconnectSheet = forwardRef<BottomSheet, Props>(
  ({ onConfirm, onCancel }, ref) => {
    const snapPoints = useMemo(() => ['42%'], []);

    const renderBackdrop = useCallback(
      (props: any) => (
        <BottomSheetBackdrop
          {...props}
          appearsOnIndex={0}
          disappearsOnIndex={-1}
          opacity={0.35}
          pressBehavior="close"
        />
      ),
      [],
    );

    const handleCancel = () => {
      (ref as any)?.current?.close();
      onCancel?.();
    };

    const handleConfirm = () => {
      (ref as any)?.current?.close();
      onConfirm?.();
    };

    return (
      <BottomSheet
        ref={ref}
        index={-1}
        snapPoints={snapPoints}
        enablePanDownToClose
        backdropComponent={renderBackdrop}
        backgroundStyle={styles.sheetBackground}
        handleIndicatorStyle={styles.handleIndicator}
      >
        <View style={styles.container}>
          <View style={styles.iconWrap}>
            <AlertTriangle size={22} color={colors.tertiary} strokeWidth={1.5} />
          </View>

          <Text weight="semibold" size={22} style={styles.title}>
            Disconnect this device?
          </Text>

          <Text size={15} color={colors.onSurfaceVariant} style={styles.body}>
            This device will stop receiving messages and cannot reconnect
            without re-pairing from a desktop.
          </Text>

          <View style={styles.actions}>
            <Pressable
              onPress={handleCancel}
              style={({ pressed }) => [
                styles.button,
                styles.buttonSecondary,
                pressed && styles.buttonPressed,
              ]}
            >
              <Text weight="medium" size={15} color={colors.onSurface}>
                Cancel
              </Text>
            </Pressable>
            <Pressable
              onPress={handleConfirm}
              style={({ pressed }) => [
                styles.button,
                styles.buttonPrimary,
                pressed && styles.buttonPressed,
              ]}
            >
              <Text
                weight="semibold"
                size={15}
                color={colors.surfaceLowest}
              >
                Disconnect
              </Text>
            </Pressable>
          </View>
        </View>
      </BottomSheet>
    );
  },
);

DisconnectSheet.displayName = 'DisconnectSheet';

const styles = StyleSheet.create({
  sheetBackground: {
    backgroundColor: colors.surfaceLowest,
    borderTopLeftRadius: radius.xl,
    borderTopRightRadius: radius.xl,
  },
  handleIndicator: {
    backgroundColor: colors.surfaceHighest,
    width: 48,
  },
  container: {
    flex: 1,
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.sm,
    gap: spacing.md,
  },
  iconWrap: {
    width: 44,
    height: 44,
    borderRadius: radius.full,
    backgroundColor: colors.tertiaryMuted,
    alignItems: 'center',
    justifyContent: 'center',
  },
  title: {
    letterSpacing: -0.4,
  },
  body: {
    lineHeight: 22,
  },
  actions: {
    flexDirection: 'row',
    gap: spacing.sm,
    marginTop: 'auto',
    marginBottom: spacing.lg,
  },
  button: {
    flex: 1,
    height: 52,
    borderRadius: radius.full,
    alignItems: 'center',
    justifyContent: 'center',
  },
  buttonSecondary: {
    backgroundColor: colors.surfaceLow,
  },
  buttonPrimary: {
    backgroundColor: colors.tertiary,
  },
  buttonPressed: {
    opacity: 0.8,
  },
});
