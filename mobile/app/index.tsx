import { useEffect, useState } from 'react';
import { Pressable, StyleSheet, View } from 'react-native';
import { useRouter } from 'expo-router';
import { useSafeAreaInsets } from 'react-native-safe-area-context';
import { CameraView, useCameraPermissions } from 'expo-camera';
import { QrCode } from 'lucide-react-native';
import { version as coreVersion } from 'pollis-native';

import { Text } from '../components/Text';
import { colors, radius, spacing } from '../theme/tokens';

// Static stub: first-launch QR scan screen. Scans pairing creds from desktop.
// No real handler — tapping "Paired" navigates to the card stack.
export default function QrScannerScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const [permission, requestPermission] = useCameraPermissions();
  const [scanned, setScanned] = useState(false);
  const [coreVer, setCoreVer] = useState<string>('…');

  useEffect(() => {
    try {
      setCoreVer(coreVersion());
    } catch (e) {
      setCoreVer(`err: ${e instanceof Error ? e.message : String(e)}`);
    }
  }, []);

  useEffect(() => {
    if (permission && !permission.granted && permission.canAskAgain) {
      requestPermission();
    }
  }, [permission, requestPermission]);

  const onBarcodeScanned = () => {
    if (scanned) {
      return;
    }
    setScanned(true);
    router.replace('/stack');
  };

  return (
    <View style={styles.root}>
      <View style={styles.cameraWrap}>
        {permission?.granted ? (
          <CameraView
            style={StyleSheet.absoluteFill}
            facing="back"
            barcodeScannerSettings={{ barcodeTypes: ['qr'] }}
            onBarcodeScanned={onBarcodeScanned}
          />
        ) : (
          <View style={[StyleSheet.absoluteFill, styles.cameraFallback]} />
        )}

        <View style={styles.viewfinderLayer} pointerEvents="none">
          <View style={styles.frame}>
            <View style={[styles.corner, styles.cornerTL]} />
            <View style={[styles.corner, styles.cornerTR]} />
            <View style={[styles.corner, styles.cornerBL]} />
            <View style={[styles.corner, styles.cornerBR]} />
          </View>
        </View>
      </View>

      <View
        style={[
          styles.bottomPanel,
          { paddingBottom: Math.max(insets.bottom + spacing.lg, spacing.xl) },
        ]}
      >
        <View style={styles.badge}>
          <QrCode size={18} color={colors.tertiary} strokeWidth={1.25} />
        </View>
        <Text weight="semibold" size={28} style={styles.title}>
          Pair this device
        </Text>
        <Text size={15} color={colors.onSurfaceVariant} style={styles.subtitle}>
          Open Pollis on your desktop, choose Pair mobile, and point your camera
          at the QR code it shows you.
        </Text>

        <Pressable
          style={({ pressed }) => [
            styles.primaryButton,
            pressed && styles.primaryButtonPressed,
          ]}
          onPress={() => router.replace('/stack')}
        >
          <Text weight="medium" size={15} color={colors.onSurface}>
            Skip for now
          </Text>
        </Pressable>
        <Text size={11} color={colors.onSurfaceVariant} style={styles.coreVersion}>
          pollis-core {coreVer}
        </Text>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    flex: 1,
    backgroundColor: colors.background,
  },
  cameraWrap: {
    flex: 1,
    backgroundColor: colors.surfaceHighest,
    overflow: 'hidden',
  },
  cameraFallback: {
    backgroundColor: colors.surfaceHighest,
  },
  viewfinderLayer: {
    ...StyleSheet.absoluteFillObject,
    alignItems: 'center',
    justifyContent: 'center',
  },
  frame: {
    width: 240,
    height: 240,
    borderRadius: radius.xl,
  },
  corner: {
    position: 'absolute',
    width: 36,
    height: 36,
    borderColor: colors.surfaceLowest,
  },
  cornerTL: {
    top: 0,
    left: 0,
    borderTopWidth: 2,
    borderLeftWidth: 2,
    borderTopLeftRadius: radius.xl,
  },
  cornerTR: {
    top: 0,
    right: 0,
    borderTopWidth: 2,
    borderRightWidth: 2,
    borderTopRightRadius: radius.xl,
  },
  cornerBL: {
    bottom: 0,
    left: 0,
    borderBottomWidth: 2,
    borderLeftWidth: 2,
    borderBottomLeftRadius: radius.xl,
  },
  cornerBR: {
    bottom: 0,
    right: 0,
    borderBottomWidth: 2,
    borderRightWidth: 2,
    borderBottomRightRadius: radius.xl,
  },
  bottomPanel: {
    paddingHorizontal: spacing.lg,
    paddingTop: spacing.lg,
    backgroundColor: colors.background,
  },
  badge: {
    width: 40,
    height: 40,
    borderRadius: radius.full,
    backgroundColor: colors.tertiaryMuted,
    alignItems: 'center',
    justifyContent: 'center',
    marginBottom: spacing.md,
  },
  title: {
    marginBottom: spacing.sm,
    letterSpacing: -0.5,
  },
  subtitle: {
    lineHeight: 22,
    marginBottom: spacing.lg,
  },
  primaryButton: {
    height: 56,
    borderRadius: radius.full,
    backgroundColor: colors.surfaceHighest,
    alignItems: 'center',
    justifyContent: 'center',
  },
  primaryButtonPressed: {
    opacity: 0.75,
  },
  coreVersion: {
    marginTop: spacing.md,
    textAlign: 'center',
    letterSpacing: 0.5,
  },
});
