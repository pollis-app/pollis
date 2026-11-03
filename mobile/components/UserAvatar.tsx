import { useEffect, useSyncExternalStore } from "react";
import { PixelRatio, View, type StyleProp, type ViewStyle } from "react-native";
import { Image } from "expo-image";
import { Avatar } from "./ui";
import { semantic, r } from "../theme/tokens";
import { avatarStore } from "../lib/avatarCache";
import { renditionFor } from "../lib/avatarKey";

type Size = "sm" | "md" | "lg" | number;

function points(size: Size): number {
  return typeof size === "number" ? size : size === "sm" ? 30 : size === "lg" ? 64 : 40;
}

/** The local file to draw for `userId` at `px`, or null for the letter avatar. */
export function useAvatarUri(
  userId: string | null | undefined,
  avatarKey: string | null | undefined,
  px: number,
): string | null {
  const uri = useSyncExternalStore(avatarStore.subscribe, () =>
    userId ? avatarStore.view(userId, px) : null,
  );
  useEffect(() => {
    if (!userId) {
      return;
    }
    avatarStore.noteKey(userId, avatarKey);
    avatarStore.ensure(userId, px);
  }, [userId, avatarKey, px]);
  return uri;
}

/** Warm a list's pictures before its rows mount. `size` in points, as the rows draw them. */
export function usePrefetchAvatars(
  rows: Array<{ userId: string; key?: string | null }>,
  size: Size,
): void {
  const px = renditionFor(points(size) * PixelRatio.get());
  const sig = rows.map((r) => `${r.userId}:${r.key ?? ""}`).join(",");
  useEffect(() => {
    if (rows.length > 0) {
      avatarStore.prefetch(rows, px);
    }
    // `sig` captures the rows' identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig, px]);
}

/**
 * A user's profile picture, or the letter avatar while there is none on disk.
 * Once a picture has been drawn it is on disk, so later renders, screens and launches
 * draw it on their first frame instead of the letter.
 * `avatarKey`: pass what the list carries (`undefined` when it carries nothing).
 */
export function UserAvatar({
  userId,
  avatarKey,
  label,
  size = "md",
  variant = "default",
  shape = "circle",
  style,
  testID,
}: {
  userId: string | null | undefined;
  avatarKey?: string | null;
  label?: string;
  size?: Size;
  variant?: "default" | "self" | "amber" | "solid";
  shape?: "circle" | "rounded";
  style?: StyleProp<ViewStyle>;
  testID?: string;
}) {
  const dim = points(size);
  const px = renditionFor(dim * PixelRatio.get());
  const uri = useAvatarUri(userId, avatarKey, px);
  const radius = shape === "rounded" ? r.lg : dim / 2;

  if (!uri || !userId) {
    return (
      <View testID={testID ? `${testID}-letter` : undefined} style={style}>
        <Avatar label={label} size={dim} variant={variant} shape={shape} />
      </View>
    );
  }
  return (
    <View
      testID={testID ? `${testID}-photo` : undefined}
      accessible={false}
      importantForAccessibility="no-hide-descendants"
      style={[
        {
          width: dim,
          height: dim,
          borderRadius: radius,
          overflow: "hidden",
          backgroundColor: semantic.high,
        },
        style,
      ]}
    >
      <Image
        source={{ uri }}
        style={{ width: dim, height: dim }}
        contentFit="cover"
        cachePolicy="memory"
        transition={0}
        recyclingKey={userId}
        onError={() => avatarStore.forget(userId, uri)}
      />
    </View>
  );
}
