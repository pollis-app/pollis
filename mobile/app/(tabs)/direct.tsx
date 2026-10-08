import { useCallback, useMemo, useState } from "react";
import { View } from "react-native";
import { useFocusEffect, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  IconButton,
  Body,
  Field,
  Button,
  Divider,
  Txt,
} from "../../components/ui";
import { ConversationRow } from "../../components/direct/ConversationRow";
import { RequestsRow } from "../../components/direct/RequestsRow";
import { conversationTime } from "../../components/direct/conversationTime";
import { Icon } from "../../components/icons";
import { semantic, space } from "../../theme/tokens";
import {
  useDMChannels,
  useDMRequests,
  useLastMessages,
  previewText,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useLayoutClass } from "../../hooks/useLayoutClass";
import { TwoPane, DetailPlaceholder } from "../../components/MasterDetail";
import { ChatView } from "../chat/[id]";

function Direct() {
  const router = useRouter();
  const { t } = useTranslation("mobile");
  const { data: dms = [], isLoading, isError } = useDMChannels();
  const { data: requests = [] } = useDMRequests();
  const setSelectedConversationId = appStore.setSelectedConversationId;
  const selectedConversationId = appStore.selectedConversationId;
  const unreadCounts = appStore.unreadCounts;
  const currentUserId = appStore.currentUser?.id;
  // On regular (iPad) width the list is the left column of a two-pane
  // master-detail; on compact it is the whole screen with push navigation.
  const isRegular = useLayoutClass() === "regular";
  const selectedDm = dms.find((d) => d.id === selectedConversationId);
  const selectedHandle = selectedDm?.user2_identifier || undefined;
  // Local filter over the conversations already loaded — no extra lookup.
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();

  // One batched preview fetch for every DM row (desktop #874/#936) — never
  // one call per row. A conversation with no locally-ingested messages is
  // absent from the map and its row simply renders without a preview.
  const dmIds = useMemo(() => dms.map((d) => d.id), [dms]);
  const { data: lastMessages = {} } = useLastMessages(dmIds);

  const shown = useMemo(() => {
    if (!needle) {
      return dms;
    }
    return dms.filter((d) => {
      const preview = previewText(lastMessages[d.id]) ?? "";
      return (
        d.user2_identifier.toLowerCase().includes(needle) ||
        preview.toLowerCase().includes(needle)
      );
    });
  }, [dms, lastMessages, needle]);

  // On compact, being on this list means no conversation is open — clear the
  // selection so realtime `new_message` events for the conversation the user
  // just left count as unread again. On regular the detail pane keeps the
  // conversation visible, so the selection (and unread suppression) stands.
  useFocusEffect(
    useCallback(() => {
      if (!isRegular) {
        appStore.setSelectedConversationId(null);
      }
    }, [isRegular]),
  );

  const openNew = () => router.push("/dm/new");

  // The single-column content — rendered as the whole screen on compact, or as
  // the left list column of the two-pane on regular. Identical on both save
  // for the row onPress, which only skips the push on regular.
  const listColumn = (
    <>
      <Header
        variant="large"
        title={t("tabs.direct")}
        actions={
          <IconButton
            testID="btn-new-dm"
            filled
            accessibilityLabel={t("direct.newMessage")}
            icon={<Icon.pencil size={20} color={semantic.text} />}
            onPress={openNew}
          />
        }
      />
      {dms.length > 0 ? (
        <View style={{ paddingHorizontal: space.xxl, paddingBottom: space.lg }}>
          <Field
            testID="input-dm-search"
            accessibilityLabel={t("direct.searchLabel")}
            placeholder={t("direct.searchLabel")}
            value={query}
            onChangeText={setQuery}
            returnKeyType="search"
            icon={<Icon.search size={18} color={semantic.muted} />}
          />
        </View>
      ) : null}
      <Body contentContainerStyle={{ paddingHorizontal: space.sm, gap: 2 }}>
        {requests.length > 0 ? (
          <>
            <RequestsRow
              testID="row-dm-requests"
              count={requests.length}
              onPress={() => router.push("/dm/requests")}
            />
            <Divider
              style={{ marginVertical: space.sm, marginHorizontal: space.sm }}
            />
          </>
        ) : null}
        {isLoading ? (
          <Txt
            variant="secondary"
            style={{ paddingHorizontal: space.sm, paddingTop: space.lg }}
          >
            {t("direct.loading")}
          </Txt>
        ) : null}
        {isError ? (
          <Txt
            variant="secondary"
            style={{ paddingHorizontal: space.sm, paddingTop: space.lg }}
          >
            {t("direct.loadFailed")}
          </Txt>
        ) : null}
        {!isLoading && !isError && dms.length === 0 ? (
          <View
            testID="direct-empty"
            style={{
              alignItems: "center",
              gap: space.lg,
              paddingHorizontal: space.xxxl,
              paddingTop: 48,
            }}
          >
            <Icon.messageCircle size={32} color={semantic.dim} />
            <Txt
              variant="heading"
              accessibilityRole="header"
              style={{ textAlign: "center" }}
            >
              {t("direct.empty")}
            </Txt>
            <Txt variant="secondary" style={{ textAlign: "center" }}>
              {t("direct.emptyHint")}
            </Txt>
            <Button
              testID="btn-new-dm-empty"
              variant="primary"
              icon={<Icon.pencil size={18} color={semantic.onAccent} />}
              onPress={openNew}
            >
              {t("direct.newMessage")}
            </Button>
          </View>
        ) : null}
        {needle && dms.length > 0 && shown.length === 0 ? (
          <Txt
            variant="secondary"
            style={{ paddingHorizontal: space.sm, paddingTop: space.lg }}
          >
            {t("direct.noMatches")}
          </Txt>
        ) : null}
        {shown.map((d) => {
          const handle = d.user2_identifier || t("dms:profile.fallbackName");
          const last = lastMessages[d.id];
          const preview = previewText(last);
          const unread = unreadCounts[d.id] ?? 0;
          return (
            <ConversationRow
              key={d.id}
              testID={`row-dm-${d.id}`}
              unreadTestID={`unread-${d.id}`}
              name={handle}
              avatarLabels={[handle]}
              preview={preview}
              own={
                !!last && !!currentUserId && last.sender_id === currentUserId
              }
              time={last ? conversationTime(last.created_at) : null}
              unread={unread}
              selected={isRegular && selectedConversationId === d.id}
              onPress={() => {
                setSelectedConversationId(d.id);
                // Opening a conversation clears its unread count (desktop
                // does the same in its DM page).
                appStore.markRead(d.id);
                // On regular the right pane updates in place; on compact push
                // the conversation as today.
                if (!isRegular) {
                  router.push({
                    pathname: "/chat/[id]",
                    params: { id: d.id, kind: "dm", name: handle },
                  });
                }
              }}
            />
          );
        })}
      </Body>
    </>
  );

  return (
    <Screen testID="screen-direct" aboveTabBar>
      {isRegular ? (
        <TwoPane
          list={listColumn}
          detail={
            selectedConversationId ? (
              <ChatView
                conversationId={selectedConversationId}
                kind="dm"
                embedded
                name={selectedHandle}
              />
            ) : (
              <DetailPlaceholder />
            )
          }
        />
      ) : (
        listColumn
      )}
    </Screen>
  );
}

export default observer(Direct);
