import { useEffect, useSyncExternalStore, type ReactElement } from "react";
import { ActivityIndicator, PixelRatio, View, type StyleProp, type ViewStyle } from "react-native";
import { Image } from "expo-image";
import { useObserver } from "mobx-react-lite";
import { Avatar } from "./ui";
import { semantic, r } from "../theme/tokens";
import { avatarStore } from "../lib/avatarCache";
import { renditionFor } from "../lib/avatarKey";
import { reportAvatarTag } from "../lib/avatarRenderState";
import { awayBite, presenceDotGeometry, type PresenceStatus } from "../lib/presence";
import { presenceStore } from "../stores/presenceStore";

type Size = "sm" | "md" | "lg" | number;

function points(size: Size): number {
  return typeof size === "number" ? size : size === "sm" ? 30 : size === "lg" ? 64 : 40;
}

/** The local file to draw for `userId` at `px`, or null for the letter avatar. */
function useAvatarUri(
  userId: string | null | undefined,
  avatarKey: string | null | undefined,
  aliasKey: string | null | undefined,
  px: number,
): string | null {
  const uri = useSyncExternalStore(avatarStore.subscribe, () =>
    userId ? avatarStore.view(userId, px, aliasKey) : null,
  );
  useEffect(() => {
    if (!userId) {
      return;
    }
    avatarStore.noteKey(userId, avatarKey);
    avatarStore.ensure(userId, px, aliasKey);
  }, [userId, avatarKey, aliasKey, px]);
  return uri;
}

/** For Maestro: `tag` reaches `<Screen>`'s AvatarProbe while it is drawn. */
function useReportTag(tag: string | null): void {
  useEffect(() => {
    if (!tag) {
      return;
    }
    return reportAvatarTag(tag);
  }, [tag]);
}

/** Warm a list's pictures before its rows mount. `size` in points, as the rows draw them. */
export function usePrefetchAvatars(
  rows: Array<{ userId: string; key?: string | null; aliasKey?: string | null }>,
  size: Size,
): void {
  const px = renditionFor(points(size) * PixelRatio.get());
  const sig = rows.map((r) => `${r.userId}:${r.key ?? ""}:${r.aliasKey ?? ""}`).join(",");
  useEffect(() => {
    if (rows.length > 0) {
      avatarStore.prefetch(rows, px);
    }
    // `sig` captures the rows' identity.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig, px]);
}

/**
 * Desktop's corner dot (`ui/Avatar.tsx`): online is an accent fill ringed in the surface behind
 * the avatar; away (#2110) is the same dot with a bite out of its top corner in the ring's colour,
 * a crescent moon in the same box; offline is a hollow dot, the background ringed in the muted
 * accent. Solid colours, no glow.
 */
function PresenceDot({ size, status, ring }: { size: number; status: PresenceStatus; ring: string }) {
  const g = presenceDotGeometry(size);
  const outer = g.dot + g.ring * 2;
  const online = status !== "offline";
  const bite = awayBite(g.dot);
  return (
    <View
      pointerEvents="none"
      style={{
        position: "absolute",
        end: g.offset,
        bottom: g.offset,
        width: outer,
        height: outer,
        borderRadius: outer / 2,
        borderWidth: g.ring,
        borderColor: online ? ring : semantic.accentMuted,
        backgroundColor: online ? semantic.accent : semantic.bg,
        overflow: "hidden",
      }}
    >
      {status === "away" ? (
        <View
          style={{
            position: "absolute",
            top: bite.inset,
            start: bite.inset,
            width: bite.size,
            height: bite.size,
            borderRadius: bite.size / 2,
            backgroundColor: ring,
          }}
        />
      ) : null}
    </View>
  );
}

/**
 * Every user avatar in the app (#2059): the user's profile picture, or the letter avatar while
 * there is none on disk, with the presence dot in its corner. Groups and channels are not users
 * and use the letter `Avatar` in `ui.tsx` directly.
 * Once a picture has been drawn it is on disk, so later renders, screens and launches draw it on
 * their first frame instead of the letter.
 * `avatarKey`: pass what the list carries (`undefined` when it carries nothing).
 * `aliasKey`: the user's alias picture in the group being drawn (#2003); it never replaces their
 * main picture anywhere else.
 * `previewUri`: a picture just picked and not yet saved (an editor), drawn instead, under a
 * spinner while `busy`.
 * `presence`: false only where the avatar stands for a conversation rather than one person (a
 * group DM's stacked pair). `on`: the surface the avatar sits on, so the dot's ring matches it.
 */
export function UserAvatar({
  userId,
  avatarKey,
  aliasKey,
  label,
  size = "md",
  variant = "default",
  shape = "circle",
  style,
  testID,
  previewUri,
  busy = false,
  presence = true,
  on = "bg",
}: {
  userId: string | null | undefined;
  avatarKey?: string | null;
  aliasKey?: string | null;
  label?: string;
  size?: Size;
  variant?: "default" | "self" | "amber" | "solid";
  shape?: "circle" | "rounded";
  style?: StyleProp<ViewStyle>;
  testID?: string;
  previewUri?: string | null;
  busy?: boolean;
  presence?: boolean;
  on?: "bg" | "panel" | "raised" | "high";
}) {
  const dim = points(size);
  const px = renditionFor(dim * PixelRatio.get());
  const stored = useAvatarUri(userId, avatarKey, aliasKey, px);
  const status = useObserver(() => presenceStore.status(userId));
  const radius = shape === "rounded" ? r.lg : dim / 2;
  const uri = previewUri || (userId ? stored : null);
  const drawsStored = !previewUri && !!uri;
  const drawsDot = presence && !!userId;

  useReportTag(drawsStored && testID ? `${testID}-photo` : null);
  useReportTag(drawsDot && testID ? `presence-${status}-${testID}` : null);

  let face: ReactElement;
  if (!uri) {
    face = (
      <Avatar
        testID={testID ? `${testID}-letter` : undefined}
        label={label}
        size={dim}
        variant={variant}
        shape={shape}
        style={[
          // On `high`, the letter avatar's own fill: one step down keeps it visible.
          on === "high" && variant === "default" ? { backgroundColor: semantic.raised } : undefined,
          drawsDot ? undefined : style,
        ]}
      />
    );
  } else {
    face = (
      <View
        testID={testID ? (previewUri ? `${testID}-preview` : `${testID}-photo`) : undefined}
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
          drawsDot ? undefined : style,
        ]}
      >
        <Image
          source={{ uri }}
          style={{ width: dim, height: dim }}
          contentFit="cover"
          cachePolicy="memory"
          transition={0}
          recyclingKey={previewUri ? undefined : (userId ?? undefined)}
          onError={drawsStored && userId ? () => avatarStore.forget(userId, uri) : undefined}
        />
        {previewUri && busy ? (
          <View
            style={{
              position: "absolute",
              top: 0,
              bottom: 0,
              start: 0,
              end: 0,
              alignItems: "center",
              justifyContent: "center",
              backgroundColor: semantic.backdrop,
            }}
          >
            <ActivityIndicator color={semantic.text} />
          </View>
        ) : null}
      </View>
    );
  }

  if (!drawsDot) {
    return face;
  }
  return (
    <View
      accessible={false}
      importantForAccessibility="no-hide-descendants"
      style={[{ width: dim, height: dim }, style]}
    >
      {face}
      <PresenceDot
        size={dim}
        status={status}
        ring={semantic[on]}
      />
    </View>
  );
}
