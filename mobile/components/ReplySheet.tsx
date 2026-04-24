import { forwardRef, useCallback, useMemo, useState } from 'react';
import { Pressable, StyleSheet, View } from 'react-native';
import BottomSheet, {
  BottomSheetBackdrop,
  BottomSheetTextInput,
} from '@gorhom/bottom-sheet';
import { Send } from 'lucide-react-native';

import { Text } from './Text';
import { colors, radius, spacing, fonts } from '../theme/tokens';

const QUICK_REPLIES = ['👍', 'On it', 'Thanks', 'Sounds good'];

interface Props {
  onSend?: () => void;
  onClose?: () => void;
}

export const ReplySheet = forwardRef<BottomSheet, Props>(
  ({ onSend, onClose }, ref) => {
    const snapPoints = useMemo(() => ['45%'], []);
    const [draft, setDraft] = useState('');

    const renderBackdrop = useCallback(
      (props: any) => (
        <BottomSheetBackdrop
          {...props}
          appearsOnIndex={0}
          disappearsOnIndex={-1}
          opacity={0.25}
          pressBehavior="close"
        />
      ),
      [],
    );

    const handleSend = () => {
      setDraft('');
      onSend?.();
    };

    const handleQuickReply = (reply: string) => {
      setDraft(reply);
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
        onClose={onClose}
      >
        <View style={styles.container}>
          <Text
            size={11}
            weight="medium"
            color={colors.onSurfaceVariant}
            style={styles.label}
          >
            QUICK REPLY
          </Text>

          <View style={styles.pillsRow}>
            {QUICK_REPLIES.map((reply) => (
              <Pressable
                key={reply}
                onPress={() => handleQuickReply(reply)}
                style={({ pressed }) => [
                  styles.pill,
                  pressed && styles.pillPressed,
                ]}
              >
                <Text size={15} weight="medium" color={colors.onSurface}>
                  {reply}
                </Text>
              </Pressable>
            ))}
          </View>

          <View style={styles.inputRow}>
            <BottomSheetTextInput
              value={draft}
              onChangeText={setDraft}
              placeholder="Type a reply…"
              placeholderTextColor={colors.onSurfaceVariant}
              style={styles.input}
              multiline
            />
            <Pressable
              onPress={handleSend}
              style={({ pressed }) => [
                styles.sendButton,
                pressed && styles.sendButtonPressed,
              ]}
            >
              <Send size={20} color={colors.surfaceLowest} strokeWidth={1.5} />
            </Pressable>
          </View>
        </View>
      </BottomSheet>
    );
  },
);

ReplySheet.displayName = 'ReplySheet';

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
    paddingTop: spacing.md,
  },
  label: {
    letterSpacing: 1.5,
    marginBottom: spacing.md,
  },
  pillsRow: {
    flexDirection: 'row',
    flexWrap: 'wrap',
    gap: spacing.sm,
    marginBottom: spacing.lg,
  },
  pill: {
    paddingHorizontal: spacing.md,
    paddingVertical: 10,
    borderRadius: radius.full,
    backgroundColor: colors.surfaceLow,
  },
  pillPressed: {
    backgroundColor: colors.surfaceHighest,
  },
  inputRow: {
    flexDirection: 'row',
    alignItems: 'flex-end',
    gap: spacing.sm,
    backgroundColor: colors.surfaceLow,
    borderRadius: radius.xl,
    paddingLeft: spacing.md,
    paddingRight: spacing.xs,
    paddingVertical: spacing.xs,
    minHeight: 56,
  },
  input: {
    flex: 1,
    fontFamily: fonts.regular,
    fontSize: 16,
    color: colors.onSurface,
    paddingVertical: spacing.sm,
    maxHeight: 120,
  },
  sendButton: {
    width: 44,
    height: 44,
    borderRadius: radius.full,
    backgroundColor: colors.tertiary,
    alignItems: 'center',
    justifyContent: 'center',
  },
  sendButtonPressed: {
    opacity: 0.8,
  },
});
