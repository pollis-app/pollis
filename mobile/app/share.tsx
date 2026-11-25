// "Send to" (#2100): where a share from another app's share sheet lands on Android, after the
// PIN. Up to 5 chats (the Vault among them), one optional message pre-filled with the shared text
// or link, and the shared files. Rows and order come from pollis-core `share_targets`, the same
// query the iOS share extension lists. Send stores the share for every chat (each its own
// message, under its own keys) and then delivers in the background: the outbox finishes anything
// that cannot go at once.

import { useMemo, useState } from "react";
import { FlatList, View } from "react-native";
import { Image } from "expo-image";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { Text } from "../components/Text";
import { Screen, Header, Field, ListRow, Button, EmptyState, Avatar, BottomAction } from "../components/ui";
import { UserAvatar } from "../components/UserAvatar";
import { Icon } from "../components/icons";
import { semantic, space, r, type as ty } from "../theme/tokens";
import { useThemeRerender } from "../components/theme";
import { useSubmitLock } from "../hooks/useSubmitLock";
import { useOpenConversation } from "../hooks/useOpenConversation";
import { useShareTargets, type ShareTarget } from "../hooks/queries/useShareTargets";
import { vaultQueryKeys } from "../hooks/queries/useVault";
import { invoke } from "../lib/native";
import { heldShare, releaseShare } from "../lib/share/pending";
import { discardShare } from "../lib/share/native";
import { videoFrame } from "../lib/media/videoFrame";
import {
  afterSend,
  canSend,
  MAX_TARGETS,
  shareRequest,
  toggleTarget,
  type ShareDraft,
} from "../lib/share/shareRules";

const THUMB = 64;

function TargetGlyph({ target }: { target: ShareTarget }) {
  if (target.kind === "vault") {
    return (
      <View
        style={{
          width: 40,
          height: 40,
          borderRadius: r.lg,
          backgroundColor: semantic.raised,
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <Icon.vault size={20} color={semantic.dim} />
      </View>
    );
  }
  if (target.kind === "dm" && target.avatar_user_id) {
    return <UserAvatar userId={target.avatar_user_id} label={target.title} />;
  }
  return <Avatar label={target.title} shape="rounded" />;
}

function ItemStrip({ draft }: { draft: ShareDraft }) {
  const { t } = useTranslation("mobile");
  if (draft.items.length === 0) {
    return null;
  }
  return (
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.sm }} testID="share-items">
      {draft.items.map((item, i) =>
        item.kind === "image" ? (
          <Image
            key={item.path}
            testID={`share-item-${i}`}
            source={{ uri: `file://${item.path}` }}
            style={{ width: THUMB, height: THUMB, borderRadius: r.md }}
            contentFit="cover"
            accessibilityLabel={item.name}
          />
        ) : (
          <View
            key={item.path}
            testID={`share-item-${i}`}
            accessible
            accessibilityLabel={item.name}
            style={{
              minHeight: THUMB,
              maxWidth: 220,
              paddingHorizontal: space.lg,
              borderRadius: r.md,
              backgroundColor: semantic.raised,
              flexDirection: "row",
              alignItems: "center",
              gap: space.sm,
            }}
          >
            {item.kind === "video" ? (
              <Icon.video size={18} color={semantic.dim} />
            ) : (
              <Icon.file size={18} color={semantic.dim} />
            )}
            <View style={{ flexShrink: 1 }}>
              <Text numberOfLines={1} style={ty.secondary}>
                {item.name}
              </Text>
              <Text style={[ty.meta, { color: semantic.muted }]}>
                {item.kind === "video" ? t("share.itemVideo") : t("share.itemFile")}
              </Text>
            </View>
          </View>
        ),
      )}
    </View>
  );
}

export default function ShareScreen() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const queryClient = useQueryClient();
  const { openConversation } = useOpenConversation();
  const params = useLocalSearchParams<{ id?: string }>();
  const draft = useMemo(() => heldShare(params.id), [params.id]);

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [capped, setCapped] = useState(false);
  const [message, setMessage] = useState(draft?.message ?? "");
  const [failed, setFailed] = useState(false);
  const targets = useShareTargets(query);
  const known = useMemo(() => {
    const map = new Map<string, ShareTarget>();
    for (const row of targets.data ?? []) {
      map.set(row.conversation_id, row);
    }
    return map;
  }, [targets.data]);
  // Rows chosen under an earlier search stay known, so the pills and the send keep them.
  const [chosen, setChosen] = useState<Map<string, ShareTarget>>(new Map());

  const send = useSubmitLock();

  const leave = () => {
    if (draft) {
      const id = draft.id;
      // Released once native no longer offers it, so a resume in between does not take it again.
      void discardShare(id).finally(() => releaseShare(id));
    }
    if (router.canGoBack()) {
      router.back();
    } else {
      router.replace("/(tabs)/direct");
    }
  };

  const onToggle = (row: ShareTarget) => {
    const next = toggleTarget(selected, row.conversation_id, MAX_TARGETS);
    setCapped(next.capped);
    setSelected(next.selected);
    setChosen((prev) => {
      const map = new Map(prev);
      map.set(row.conversation_id, row);
      return map;
    });
  };

  const onSend = async () => {
    if (!canSend(selected, message, draft) || !send.begin()) {
      return;
    }
    setFailed(false);
    try {
      const shareId = await invoke<string>("new_message_id");
      // A video's frame for its poster (#1837), as the composer sends one.
      const posters: Record<string, string> = {};
      for (const item of draft?.items ?? []) {
        if (item.kind === "video") {
          const frame = await videoFrame(`file://${item.path}`);
          if (frame) {
            posters[item.path] = frame;
          }
        }
      }
      const request = shareRequest(shareId, selected, message, draft, posters);
      const queued = await invoke<{ conversation_id: string; message_id: string }[]>("share_enqueue", {
        request,
      });
      // Stored for every chat: the copies are in the encrypted database now.
      if (draft) {
        const id = draft.id;
        void discardShare(id).finally(() => releaseShare(id));
      }
      // Delivered in the background; anything that cannot go now, the outbox finishes.
      void invoke("share_deliver", { queued })
        .catch(() => undefined)
        .finally(() => {
          void queryClient.invalidateQueries();
        });
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
      const rows = selected.map((id) => chosen.get(id) ?? known.get(id));
      const next = afterSend(
        rows.filter((x): x is ShareTarget => !!x).map((x) => ({ conversationId: x.conversation_id, kind: x.kind })),
      );
      if (next.to === "chat") {
        openConversation({ id: next.conversationId, kind: next.kind }, { replace: true });
      } else if (next.to === "vault") {
        router.replace("/vault");
      } else if (router.canGoBack()) {
        router.back();
      } else {
        router.replace("/(tabs)/direct");
      }
    } catch {
      setFailed(true);
      send.end();
    }
  };

  const rows = targets.data ?? [];
  const ready = canSend(selected, message, draft);

  return (
    <Screen testID="screen-share">
      <Header title={t("share.title")} onBack={leave} backLabel={t("share.cancel")} />
      <FlatList
        data={rows}
        keyExtractor={(row) => row.conversation_id}
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{ paddingBottom: space.xxxl }}
        ListHeaderComponent={
          <View style={{ padding: space.xxl, gap: space.lg }}>
            {draft ? <ItemStrip draft={draft} /> : null}
            {draft && draft.tooLarge.length > 0 ? (
              <Text testID="share-too-large" style={[ty.meta, { color: semantic.danger }]}>
                {draft.tooLarge.map((name) => t("share.tooLarge", { name })).join("\n")}
              </Text>
            ) : null}
            {draft && draft.unreadable > 0 ? (
              <Text testID="share-unreadable" style={[ty.meta, { color: semantic.danger }]}>
                {t("share.unreadable")}
              </Text>
            ) : null}
            {draft?.truncated ? (
              <Text testID="share-too-many" style={[ty.meta, { color: semantic.muted }]}>
                {t("share.tooMany")}
              </Text>
            ) : null}
            <Field
              value={message}
              onChangeText={setMessage}
              placeholder={t("share.messagePlaceholder")}
              accessibilityLabel={t("share.messagePlaceholder")}
              testID="input-share-message"
              multiline
              autoCapitalize="sentences"
            />
            <Field
              value={query}
              onChangeText={setQuery}
              placeholder={t("share.search")}
              accessibilityLabel={t("share.search")}
              testID="input-share-search"
              autoCorrect={false}
              returnKeyType="search"
              icon={<Icon.search size={18} color={semantic.muted} />}
            />
            {capped ? (
              <Text testID="share-max-targets" style={[ty.meta, { color: semantic.muted }]}>
                {t("share.maxTargets")}
              </Text>
            ) : null}
          </View>
        }
        renderItem={({ item }) => {
          const on = selected.includes(item.conversation_id);
          const title = item.kind === "vault" ? t("share.vault") : item.kind === "channel" ? `#${item.title}` : item.title;
          const sub =
            item.kind === "vault"
              ? t("share.vaultSub")
              : item.kind === "channel" && item.subtitle
                ? t("share.channelIn", { group: item.subtitle })
                : (item.subtitle ?? undefined);
          return (
            <ListRow
              testID={`share-target-${item.kind === "vault" ? "vault" : item.title}`}
              glyph={<TargetGlyph target={item} />}
              name={title}
              sub={sub}
              selected={on}
              end={on ? <Icon.check size={20} color={semantic.accent} /> : undefined}
              onPress={() => onToggle(item)}
            />
          );
        }}
        ListEmptyComponent={
          targets.isError ? (
            <EmptyState
              testID="share-load-failed"
              action={
                <Button variant="subtle" onPress={() => void targets.refetch()} testID="btn-share-retry">
                  {t("share.retry")}
                </Button>
              }
            >
              {t("share.loadFailed")}
            </EmptyState>
          ) : targets.isPending ? (
            <EmptyState testID="share-loading">{t("share.loading")}</EmptyState>
          ) : (
            <EmptyState testID="share-empty">{query.trim() ? t("share.noMatches") : t("share.noChats")}</EmptyState>
          )
        }
      />
      <BottomAction>
        {failed ? (
          <Text accessibilityRole="alert" testID="share-send-failed" style={[ty.meta, { color: semantic.danger }]}>
            {t("share.sendFailed")}
          </Text>
        ) : null}
        {selected.length > 0 ? (
          <Text testID="share-selected-count" style={[ty.meta, { color: semantic.muted }]}>
            {t("share.selected", { count: selected.length })}
          </Text>
        ) : null}
        <Button
          variant="primary"
          full
          testID="btn-share-send"
          disabled={!ready}
          loading={send.busy}
          onPress={() => void onSend()}
        >
          {send.busy ? t("share.sending") : t("share.send")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
