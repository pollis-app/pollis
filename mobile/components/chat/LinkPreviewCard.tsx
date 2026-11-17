// A sender-generated link preview (#1987), compact: text at the start, a square thumbnail at the end.

import type { ReactNode } from "react";
import { Pressable, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Text } from "../Text";
import { Icon } from "../icons";
import { IconButton } from "../ui";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";
import { fonts, r, semantic, space, type as ty } from "../../theme/tokens";

// The thumbnail's fixed square, so a picture arriving late never moves the message under it.
export const LINK_PREVIEW_THUMB = 64;

/**
 * Plain text only: title and description are never parsed. `host` is the BODY link's host,
 * never anything from the preview. A received card opens that link on a tap; a composer card
 * carries the × instead.
 */
export function LinkPreviewCard({
  host,
  title,
  description,
  thumb,
  loading = false,
  onPress,
  onLongPress,
  onRemove,
  testID,
}: {
  host: string;
  title: string;
  description?: string | null;
  thumb?: ReactNode;
  loading?: boolean;
  onPress?: () => void;
  // The row's long press, so pressing the card still opens the message sheet.
  onLongPress?: () => void;
  onRemove?: () => void;
  testID?: string;
}) {
  const { t } = useTranslation("chat");

  const body = (
    <>
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <Text numberOfLines={1} style={[ty.meta, { color: semantic.muted, writingDirection: "ltr" }]}>
          {host}
        </Text>
        {loading ? (
          <Text numberOfLines={1} style={[ty.secondary, { color: semantic.muted }]}>
            {t("linkPreview.loading")}
          </Text>
        ) : (
          <>
            <Text
              numberOfLines={1}
              style={[ty.secondary, { fontFamily: fonts.semibold, color: semantic.text }]}
            >
              {title}
            </Text>
            {description ? (
              <Text numberOfLines={2} style={[ty.meta, { color: semantic.dim }]}>
                {description}
              </Text>
            ) : null}
          </>
        )}
      </View>
      {thumb ? (
        <View
          style={{
            width: LINK_PREVIEW_THUMB,
            height: LINK_PREVIEW_THUMB,
            borderRadius: r.sm,
            overflow: "hidden",
            backgroundColor: semantic.high,
          }}
        >
          {thumb}
        </View>
      ) : null}
    </>
  );

  const frame = {
    flexDirection: "row" as const,
    alignItems: "center" as const,
    gap: space.lg,
    minHeight: LINK_PREVIEW_THUMB + 2 * space.md,
    paddingVertical: space.md,
    paddingStart: space.lg,
    paddingEnd: onRemove ? 0 : space.md,
    borderRadius: r.md,
    borderWidth: 1,
    borderColor: semantic.hair,
  };

  if (onPress) {
    return (
      <Pressable
        testID={testID}
        onPress={onPress}
        onLongPress={onLongPress}
        delayLongPress={LONG_PRESS_MS}
        accessibilityRole="link"
        accessibilityLabel={`${title}, ${host}`}
        style={({ pressed }) => [
          frame,
          { backgroundColor: pressed ? semantic.high : semantic.raised },
        ]}
      >
        {body}
      </Pressable>
    );
  }

  return (
    <View
      testID={testID}
      accessibilityState={{ busy: loading }}
      style={[frame, { backgroundColor: semantic.raised }]}
    >
      {body}
      {onRemove ? (
        <IconButton
          testID="btn-link-preview-remove"
          accessibilityLabel={t("linkPreview.remove")}
          icon={<Icon.close size={20} />}
          tone="neutral"
          onPress={onRemove}
        />
      ) : null}
    </View>
  );
}
