// Full-screen media viewer (#1248) — mobile's port of desktop's attachment
// lightbox (AttachmentDisplay) and its walkable roll (MediaGalleryView).
//
// A pushed page, never a modal: Close/back at the top-start, the system edge
// swipe pops it (except while an image is zoomed, when a drag pans instead).
// An image or video walks the conversation's images and videos, by swipe or
// with the previous/next buttons (desktop's arrows, wrapping like desktop's
// roll); audio and other files open alone. Save to photos (images, video)
// and Share (anything) replace desktop's Download.
//
// Every byte shown here is the locally decrypted file the chat already uses
// (`useMediaUri` / `get_media_path`); nothing is fetched in plaintext.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AccessibilityInfo, Linking, Text, View } from "react-native";
import { FlatList } from "react-native-gesture-handler";
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
  type ConversationKind,
} from "../hooks/queries";
import { releaseNamedMediaUri, resolveNamedMediaUri } from "../lib/media/cache";
import { saveToPhotoLibrary, shareFile } from "../lib/media/export";
import {
  canSaveToLibrary,
  collectViewerItems,
  currentAttachmentId,
  mediaKind,
} from "../lib/media/viewer";
import { formatBytes } from "../lib/exportArchive";
import { semantic, space, type as ty } from "../theme/tokens";
import type { MessageAttachment } from "../types";

type Feedback = "saved" | "denied" | "saveFailed" | "shareFailed" | "shareUnavailable";

const FEEDBACK_KEYS: Record<Feedback, string> = {
  saved: "media.saved",
  denied: "media.saveDenied",
  saveFailed: "media.saveFailed",
  shareFailed: "media.shareFailed",
  shareUnavailable: "media.shareUnavailable",
};

export default function MediaViewerScreen() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const navigation = useNavigation();
  const params = useLocalSearchParams<{
    conversationId?: string;
    kind?: string;
    attachmentId?: string;
    threadId?: string;
  }>();
  const conversationId = params.conversationId ?? null;
  const kind: ConversationKind | null =
    params.kind === "channel" || params.kind === "dm" ? params.kind : null;
  const threadId = params.threadId ?? null;
  const attachmentId = params.attachmentId ?? "";

  // The same caches the chat and thread screens fill — no new reads.
  const { data: conversationData } = useMessages(conversationId, kind);
  const { data: replies } = useThreadMessages(threadId);
  const messages = useMemo(() => {
    const conversation = flattenPages(conversationData);
    if (!threadId) {
      return conversation;
    }
    // A thread's roll is its root plus its replies (replies are filtered
    // out of the main timeline).
    const root = conversation.find((m) => m.id === threadId);
    return [...(root ? [root] : []), ...(replies ?? [])];
  }, [conversationData, replies, threadId]);

  // Both ids follow a pending → confirmed swap (`currentAttachmentId`): the
  // viewer may open on a just-sent attachment before its send settles. The
  // swap lands with the cache update that re-renders this screen.
  const { items, index: initialIndex } = useMemo(
    () => collectViewerItems(messages, currentAttachmentId(attachmentId)),
    [messages, attachmentId],
  );

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
  const shared = useRef(new Map<string, MessageAttachment>());
  useEffect(() => {
    const held = shared.current;
    return () => {
      for (const attachment of held.values()) {
        void releaseNamedMediaUri(attachment);
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
      const uri = await resolveNamedMediaUri(current);
      try {
        const result = await saveToPhotoLibrary(uri);
        announce(result === "saved" ? "saved" : "denied");
      } finally {
        // The library has its own copy once the save resolves.
        void releaseNamedMediaUri(current);
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
      const uri = await resolveNamedMediaUri(current);
      if (shared.current.has(current.id)) {
        // Already held from an earlier share: drop the extra reference.
        void releaseNamedMediaUri(current);
      } else {
        shared.current.set(current.id, current);
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
        onBack={() => router.back()}
      />
      <View
        testID="pager-media"
        style={{ flex: 1 }}
        onLayout={(e) => {
          const { width, height } = e.nativeEvent.layout;
          setSize({ width, height });
        }}
      >
        {items.length === 0 ? (
          <MediaStatus state={conversationData ? "error" : "loading"} />
        ) : size ? (
          <FlatList
            ref={listRef}
            data={items}
            keyExtractor={(a) => a.id}
            horizontal
            pagingEnabled
            showsHorizontalScrollIndicator={false}
            scrollEnabled={multiple && !zoomed}
            initialScrollIndex={initialIndex}
            getItemLayout={(_, index) => ({
              length: size.width,
              offset: size.width * index,
              index,
            })}
            // One page each side: neighbours decrypt ahead of the swipe, and
            // nothing further away holds plaintext on disk.
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
        ) : null}
      </View>

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
