// Full-screen media viewer (#1248, #1492): a pushed page, never a modal. A
// page decrypts on mount and unlinks its plaintext file on unmount.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AccessibilityInfo, FlatList, Linking, Text, View } from "react-native";
import { Gesture, GestureDetector } from "react-native-gesture-handler";
import Animated, { useAnimatedStyle, useSharedValue, withTiming } from "react-native-reanimated";
import { scheduleOnRN } from "react-native-worklets";
import { useLocalSearchParams, useNavigation, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Button, Header, IconButton, Screen } from "../components/ui";
import { Icon } from "../components/icons";
import { MediaViewerPage } from "../components/media/MediaViewerPage";
import { MediaStatus } from "../components/media/MediaStatus";
import {
  flattenPages,
  useMessages,
  useThreadMessages,
  useVaultMessages,
  type ConversationKind,
} from "../hooks/queries";
import { vaultViewerItems } from "../lib/vault";
import {
  mediaCacheGeneration,
  releaseNamedMediaUri,
  resolveNamedMediaUri,
} from "../lib/media/cache";
import { saveToPhotoLibrary, shareFile } from "../lib/media/export";
import {
  canSaveToLibrary,
  collectViewerItems,
  currentAttachmentId,
  mediaKind,
  shouldDismiss,
} from "../lib/media/viewer";
import { formatBytes } from "../lib/exportArchive";
import { semantic, space, type as ty } from "../theme/tokens";
import type { MessageAttachment } from "../types";
import { useThemeRerender } from "../components/theme";

type Feedback = "saved" | "denied" | "saveFailed" | "shareFailed" | "shareUnavailable";

const FEEDBACK_KEYS: Record<Feedback, string> = {
  saved: "media.saved",
  denied: "media.saveDenied",
  saveFailed: "media.saveFailed",
  shareFailed: "media.shareFailed",
  shareUnavailable: "media.shareUnavailable",
};

export default function MediaViewerScreen() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const navigation = useNavigation();
  const params = useLocalSearchParams<{
    conversationId?: string;
    kind?: string;
    attachmentId?: string;
    threadId?: string;
    // "vault": walk the Vault's entries (#1316) instead of a conversation.
    source?: string;
  }>();
  const fromVault = params.source === "vault";
  const conversationId = params.conversationId ?? null;
  const kind: ConversationKind | null =
    params.kind === "channel" || params.kind === "dm" ? params.kind : null;
  const threadId = params.threadId ?? null;
  const attachmentId = params.attachmentId ?? "";

  // The same caches the chat and thread screens fill — no new reads.
  const { data: conversationData } = useMessages(conversationId, kind);
  const { data: replies } = useThreadMessages(threadId);
  const { data: vaultEntries } = useVaultMessages(fromVault);
  const messages = useMemo(() => {
    if (fromVault) {
      return [];
    }
    const conversation = flattenPages(conversationData);
    if (!threadId) {
      return conversation;
    }
    // A thread's roll is its root plus its replies (replies are filtered
    // out of the main timeline).
    const root = conversation.find((m) => m.id === threadId);
    return [...(root ? [root] : []), ...(replies ?? [])];
  }, [conversationData, replies, threadId, fromVault]);

  // `currentAttachmentId` follows a pending → confirmed swap: the viewer may
  // open on a just-sent attachment before its send settles. From the Vault
  // the roll is the grid's, in grid order (#1492).
  const { items, index: initialIndex } = useMemo(
    () =>
      fromVault
        ? vaultViewerItems(vaultEntries ?? [], currentAttachmentId(attachmentId))
        : collectViewerItems(messages, currentAttachmentId(attachmentId)),
    [fromVault, vaultEntries, messages, attachmentId],
  );
  const sourceLoaded = fromVault ? !!vaultEntries : !!conversationData;

  // Track the page by attachment id, not index: a message arriving or being
  // deleted while the viewer is open shifts indices, not identities.
  const [trackedId, setCurrentId] = useState(attachmentId);
  const currentId = currentAttachmentId(trackedId);
  const foundIndex = items.findIndex((a) => a.id === currentId);
  const currentIndex = foundIndex >= 0 ? foundIndex : 0;
  const current: MessageAttachment | null = items[currentIndex] ?? null;
  const multiple = items.length > 1;

  const [size, setSize] = useState<{ width: number; height: number } | null>(null);
  const [zoomed, setZoomed] = useState(false);
  const [busy, setBusy] = useState<"save" | "share" | null>(null);
  const [feedback, setFeedback] = useState<Feedback | null>(null);
  const listRef = useRef<FlatList<MessageAttachment>>(null);

  // A zoomed image pans with a drag, so the edge swipe must not pop the
  // page under it.
  useEffect(() => {
    navigation.setOptions({ gestureEnabled: !zoomed });
  }, [navigation, zoomed]);

  // Feedback belongs to the item it was about, and a new page starts at 1×
  // (the previous page resets its own zoom when it goes inactive).
  useEffect(() => {
    setFeedback(null);
    setZoomed(false);
  }, [currentId]);

  // Named copies handed to the share sheet stay on disk until the viewer
  // closes: the receiving app may read the file after the sheet returns.
  // Each keeps the cache generation it was taken in, so a close after a
  // sign-out clear releases nothing.
  const shared = useRef(new Map<string, { attachment: MessageAttachment; resolvedIn: number }>());
  useEffect(() => {
    const held = shared.current;
    return () => {
      for (const { attachment, resolvedIn } of held.values()) {
        void releaseNamedMediaUri(attachment, resolvedIn);
      }
      held.clear();
    };
  }, []);

  const announce = useCallback(
    (next: Feedback) => {
      setFeedback(next);
      AccessibilityInfo.announceForAccessibility(t(FEEDBACK_KEYS[next]));
    },
    [t],
  );

  const onSave = useCallback(async () => {
    if (!current || busy) {
      return;
    }
    setBusy("save");
    setFeedback(null);
    try {
      const resolvedIn = mediaCacheGeneration();
      const uri = await resolveNamedMediaUri(current);
      try {
        const result = await saveToPhotoLibrary(uri);
        announce(result === "saved" ? "saved" : "denied");
      } finally {
        // The library has its own copy once the save resolves.
        void releaseNamedMediaUri(current, resolvedIn);
      }
    } catch {
      announce("saveFailed");
    } finally {
      setBusy(null);
    }
  }, [current, busy, announce]);

  const onShare = useCallback(async () => {
    if (!current || busy) {
      return;
    }
    setBusy("share");
    setFeedback(null);
    try {
      const resolvedIn = mediaCacheGeneration();
      const uri = await resolveNamedMediaUri(current);
      if (shared.current.has(current.id)) {
        // Already held from an earlier share: drop the extra reference.
        void releaseNamedMediaUri(current, resolvedIn);
      } else {
        shared.current.set(current.id, { attachment: current, resolvedIn });
      }
      const ok = await shareFile(uri, current.content_type, current.filename);
      if (!ok) {
        announce("shareUnavailable");
      }
    } catch {
      announce("shareFailed");
    } finally {
      setBusy(null);
    }
  }, [current, busy, announce]);

  const step = useCallback(
    (delta: number) => {
      if (items.length < 2) {
        return;
      }
      const next = (currentIndex + delta + items.length) % items.length;
      setCurrentId(items[next].id);
      listRef.current?.scrollToIndex({
        index: next,
        animated: Math.abs(next - currentIndex) === 1,
      });
    },
    [items, currentIndex],
  );

  // Swipe down to dismiss (#1492). It fails once the drag turns sideways, so
  // a horizontal swipe always pages; one finger, so a pinch stays the image's.
  const pagerGesture = useMemo(() => Gesture.Native(), []);
  const dragY = useSharedValue(0);
  const pageHeight = size?.height ?? 0;
  const close = useCallback(() => {
    router.back();
  }, [router]);
  const dismissGesture = Gesture.Pan()
    .enabled(!zoomed && pageHeight > 0)
    .maxPointers(1)
    .activeOffsetY(24)
    .failOffsetY(-24)
    .failOffsetX([-16, 16])
    .simultaneousWithExternalGesture(pagerGesture)
    .onUpdate((e) => {
      dragY.value = Math.max(0, e.translationY);
    })
    .onEnd((e) => {
      if (shouldDismiss(dragY.value, e.velocityY, pageHeight)) {
        dragY.value = withTiming(pageHeight, { duration: 160 }, (finished) => {
          if (finished) {
            scheduleOnRN(close);
          }
        });
      } else {
        dragY.value = withTiming(0, { duration: 180 });
      }
    });
  const dragStyle = useAnimatedStyle(() => {
    const progress = pageHeight > 0 ? Math.min(1, dragY.value / pageHeight) : 0;
    return {
      opacity: 1 - progress * 0.6,
      transform: [{ translateY: dragY.value }, { scale: 1 - progress * 0.15 }],
    };
  });

  const kindOfCurrent = current ? mediaKind(current.content_type) : "file";
  // An optimistic send still uploading has neither bytes in R2 nor, unless
  // it was picked on this device, a local file.
  const unavailable = !!current && !current.object_key && !current.localPreviewUri;
  const subtitle = current
    ? [
        multiple
          ? t("vault:media.position", { index: currentIndex + 1, total: items.length })
          : null,
        current.file_size > 0 ? formatBytes(current.file_size) : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : undefined;

  return (
    <Screen testID="screen-media-viewer" wide>
      <Header
        title={current?.filename ?? t("media.title")}
        subtitle={subtitle || undefined}
        backLabel={t("vault:media.close")}
        backTestID="btn-media-close"
        titleTestID="text-media-title"
        subtitleTestID="text-media-position"
        onBack={() => router.back()}
      />
      <GestureDetector gesture={dismissGesture}>
        <Animated.View
          testID="pager-media"
          style={[{ flex: 1 }, dragStyle]}
          onLayout={(e) => {
            const { width, height } = e.nativeEvent.layout;
            setSize({ width, height });
          }}
        >
          {items.length === 0 ? (
            <MediaStatus state={sourceLoaded ? "error" : "loading"} />
          ) : size ? (
            <GestureDetector gesture={pagerGesture}>
              <FlatList
                ref={listRef}
                data={items}
                keyExtractor={(a) => a.id}
                horizontal
                pagingEnabled
                directionalLockEnabled
                showsHorizontalScrollIndicator={false}
                scrollEnabled={multiple && !zoomed}
                initialScrollIndex={initialIndex}
                getItemLayout={(_, index) => ({
                  length: size.width,
                  offset: size.width * index,
                  index,
                })}
                // One page each side: neighbours decrypt ahead of the swipe,
                // and nothing further away holds plaintext on disk.
                windowSize={3}
                initialNumToRender={1}
                maxToRenderPerBatch={1}
                extraData={currentIndex}
                onMomentumScrollEnd={(e) => {
                  const index = Math.round(e.nativeEvent.contentOffset.x / size.width);
                  const next = items[Math.max(0, Math.min(items.length - 1, index))];
                  if (next) {
                    setCurrentId(next.id);
                  }
                }}
                renderItem={({ item, index }) => (
                  <MediaViewerPage
                    attachment={item}
                    width={size.width}
                    height={size.height}
                    active={index === currentIndex}
                    onZoomChange={setZoomed}
                  />
                )}
              />
            </GestureDetector>
          ) : null}
        </Animated.View>
      </GestureDetector>

      {current ? (
        <View
          style={{
            gap: space.md,
            paddingHorizontal: space.xxl,
            paddingTop: space.md,
            paddingBottom: space.md,
          }}
        >
          {feedback ? (
            <View
              style={{
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "center",
                flexWrap: "wrap",
                gap: space.sm,
              }}
            >
              <Text
                testID="text-media-feedback"
                accessibilityLiveRegion="polite"
                style={[ty.secondary, { color: semantic.text, textAlign: "center" }]}
              >
                {t(FEEDBACK_KEYS[feedback])}
              </Text>
              {feedback === "denied" ? (
                <Button
                  testID="btn-media-open-settings"
                  pressIn
                  variant="subtle"
                  onPress={() => {
                    void Linking.openSettings();
                  }}
                >
                  {t("media.openSettings")}
                </Button>
              ) : null}
            </View>
          ) : null}
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
            {multiple ? (
              <IconButton
                testID="btn-media-prev"
                pressIn
                filled
                accessibilityLabel={t("vault:media.previous")}
                onPress={() => step(-1)}
                icon={<Icon.chevronLeft size={22} />}
              />
            ) : null}
            <View
              style={{
                flex: 1,
                flexDirection: "row",
                justifyContent: "center",
                flexWrap: "wrap",
                gap: space.sm,
              }}
            >
              {canSaveToLibrary(kindOfCurrent) ? (
                <Button
                  testID="btn-media-save"
                  variant="primary"
                  disabled={!!busy || unavailable}
                  icon={<Icon.saveImage size={18} />}
                  onPress={() => {
                    void onSave();
                  }}
                >
                  {busy === "save" ? t("media.saving") : t("media.save")}
                </Button>
              ) : null}
              <Button
                testID="btn-media-share"
                variant={canSaveToLibrary(kindOfCurrent) ? "secondary" : "primary"}
                disabled={!!busy || unavailable}
                icon={<Icon.share size={18} />}
                onPress={() => {
                  void onShare();
                }}
              >
                {t("media.share")}
              </Button>
            </View>
            {multiple ? (
              <IconButton
                testID="btn-media-next"
                pressIn
                filled
                accessibilityLabel={t("vault:media.next")}
                onPress={() => step(1)}
                icon={<Icon.chevronRight size={22} />}
              />
            ) : null}
          </View>
        </View>
      ) : null}
    </Screen>
  );
}
