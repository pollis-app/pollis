import { useCallback, useEffect, useMemo, useState } from "react";
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
import { VaultRow } from "../../components/direct/VaultRow";
import { conversationTime } from "../../components/direct/conversationTime";
import { Icon } from "../../components/icons";
import { semantic, space } from "../../theme/tokens";
import {
  useDMChannels,
  useDMRequests,
  useLastMessages,
  previewText,
  markConversationRead,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useLayoutClass } from "../../hooks/useLayoutClass";
import {
  TwoPane,
  DetailPlaceholder,
  DetailPane,
  PaneProvider,
  CenteredColumn,
} from "../../components/MasterDetail";
import { usePaneStack } from "../../components/pane/paneContext";
import { ChatView } from "../chat/[id]";
import { useThemeRerender } from "../../components/theme";

function Direct() {
  useThemeRerender();
  const router = useRouter();
  const { t } = useTranslation("mobile");
  const { data: dms = [], isLoading, isError } = useDMChannels();
  const { data: requests = [] } = useDMRequests();
  const setSelectedConversationId = appStore.setSelectedConversationId;
  const selectedConversationId = appStore.selectedConversationId;
  const unreadCounts = appStore.unreadCounts;
  const currentUserId = appStore.currentUser?.id;
  const isRegular = useLayoutClass() === "regular";
  // The right pane's page stack; another conversation starts it over.
  const pane = usePaneStack();
  const resetPane = pane.api.reset;
  useEffect(() => {
    resetPane();
  }, [selectedConversationId, resetPane]);
  const selectedDm = dms.find((d) => d.id === selectedConversationId);
  const selectedHandle = selectedDm?.user2_identifier || undefined;
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();

  // One batched preview fetch for every row, never one call per row.
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

  // On compact no conversation is open here, so clear the selection and let
  // the one just left count unread again; on regular the pane still shows it.
  useFocusEffect(
    useCallback(() => {
      if (!isRegular) {
        appStore.setSelectedConversationId(null);
      }
    }, [isRegular]),
  );

  const openNew = () => router.push("/dm/new");

  const listColumn = (
    <>
      <Header
        variant="large"
        title={t("tabs.direct")}
        actions={
          <IconButton
            testID="btn-new-dm"
            pressIn
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
        {!needle ? (
          <>
            <VaultRow onPress={() => router.push("/vault")} />
            <Divider
              style={{ marginVertical: space.sm, marginHorizontal: space.sm }}
            />
          </>
        ) : null}
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
                // Re-opening the conversation already shown closes any page
                // over it.
                resetPane();
                markConversationRead(d.id);
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

  // A conversation just opened from elsewhere counts, ahead of the list's
  // refetch.
  const hasList =
    dms.length > 0 || requests.length > 0 || !!selectedConversationId;
  let body: React.ReactNode = listColumn;
  if (isRegular && hasList) {
    body = (
      <PaneProvider pane={pane}>
        <TwoPane
          list={listColumn}
          detail={
            <DetailPane
              pane={pane}
              root={
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
          }
        />
      </PaneProvider>
    );
  } else if (isRegular) {
    body = <CenteredColumn>{listColumn}</CenteredColumn>;
  }

  return (
    <Screen testID="screen-direct" aboveTabBar wide>
      {body}
    </Screen>
  );
}

export default observer(Direct);
