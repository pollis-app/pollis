import { useMemo, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Field,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useDMChannels,
  useSearchMessages,
  useUserGroupsWithChannels,
  useUserSearch,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { activeLocale, upper } from "../../i18n";

export default function Search() {
  const router = useRouter();
  const { t } = useTranslation("mobile");
  const [q, setQ] = useState("");
  const trimmed = q.trim();
  const messages = useSearchMessages(trimmed);
  const user = useUserSearch(trimmed);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dms = [] } = useDMChannels();

  // `search_messages` rows carry only a conversation_id — no kind — so a DM
  // hit navigated as kind:"channel" opened the wrong conversation type.
  // Resolve the kind by cross-referencing the id against the cached channel
  // and DM lists (the same sources the tabs render from).
  const conversationKinds = useMemo(() => {
    const kinds = new Map<
      string,
      { kind: "channel" | "dm"; name?: string; groupId?: string }
    >();
    for (const g of groups) {
      for (const c of g.channels) {
        kinds.set(c.id, { kind: "channel", name: c.name, groupId: g.id });
      }
    }
    for (const d of dms) {
      kinds.set(d.id, { kind: "dm", name: d.user2_identifier });
    }
    return kinds;
  }, [groups, dms]);

  // Client-side filter of cached groups/channels. The Rust DB doesn't
  // expose a single "search everything" command — desktop also stitches
  // these on the frontend.
  const filtered = useMemo(() => {
    if (trimmed.length < 2) {
      return {
        groups: [],
        channels: [] as {
          id: string;
          name: string;
          groupName: string;
          groupId: string;
        }[],
      };
    }
    const lower = trimmed.toLowerCase();
    const matchingGroups = groups.filter((g) =>
      g.name.toLowerCase().includes(lower),
    );
    const matchingChannels: {
      id: string;
      name: string;
      groupName: string;
      groupId: string;
    }[] = [];
    for (const g of groups) {
      for (const c of g.channels) {
        if (c.name.toLowerCase().includes(lower)) {
          matchingChannels.push({
            id: c.id,
            name: c.name,
            groupName: g.name,
            groupId: g.id,
          });
        }
      }
    }
    return { groups: matchingGroups, channels: matchingChannels };
  }, [groups, trimmed]);

  const totalResults =
    (user.data ? 1 : 0) +
    filtered.groups.length +
    filtered.channels.length +
    (messages.data?.results.length ?? 0);

  const showEmpty =
    trimmed.length >= 2 &&
    !messages.isLoading &&
    !user.isLoading &&
    totalResults === 0;

  return (
    <Screen testID="screen-search">
      <Crumb
        segs={[{ label: upper(t("tabs.search")), leaf: true }]}
        end={upper(
          trimmed.length >= 2
            ? t("search.resultCount", { count: totalResults })
            : t("search.typePrompt"),
        )}
      />
      <Body>
        {trimmed.length < 2 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 14,
            }}
          >
            {t("search.hint")}
          </Text>
        ) : null}
        {showEmpty ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 14,
            }}
          >
            {t("search:panel.noMatches")}
          </Text>
        ) : null}

        {filtered.groups.length > 0 ? (
          <View>
            <SectionTitle>{upper(t("tabs.groups"))}</SectionTitle>
            {filtered.groups.map((g) => (
              <ListRow
                key={g.id}
                testID={`row-group-${g.id}`}
                minHeight={46}
                glyph={<Icon.diamond size={14} color={semantic.mute} />}
                name={g.name}
                nameStyle={{ fontSize: 14, fontFamily: ty.rowN.fontFamily }}
                onPress={() =>
                  router.push({
                    pathname: "/group/[id]",
                    params: { id: g.id },
                  })
                }
              />
            ))}
          </View>
        ) : null}

        {filtered.channels.length > 0 ? (
          <View>
            <SectionTitle>{upper(t("search.channels"))}</SectionTitle>
            {filtered.channels.map((c) => (
              <ListRow
                key={c.id}
                testID={`row-channel-${c.id}`}
                minHeight={48}
                glyph={<Icon.hash color={semantic.mute} />}
                name={c.name}
                sub={c.groupName}
                onPress={() => {
                  // Mirror the groups-tab rows: select + clear unread on open.
                  appStore.setSelectedGroupId(c.groupId);
                  appStore.setSelectedChannelId(c.id);
                  appStore.markRead(c.id);
                  router.push({
                    pathname: "/chat/[id]",
                    params: { id: c.id, kind: "channel", name: c.name },
                  });
                }}
              />
            ))}
          </View>
        ) : null}

        {user.data ? (
          <View>
            <SectionTitle>{upper(t("tabs.direct"))}</SectionTitle>
            <ListRow
              testID={`row-user-${user.data.id}`}
              minHeight={48}
              glyph={
                <Avatar
                  label={(user.data.username || "us").slice(0, 2)}
                  size="sm"
                />
              }
              name={`@${user.data.username}`}
              sub={user.data.preferred_name || user.data.email || undefined}
              onPress={() =>
                router.push({
                  pathname: "/user/[id]",
                  params: { id: user.data!.id },
                })
              }
            />
          </View>
        ) : null}

        {(messages.data?.results.length ?? 0) > 0 ? (
          <View>
            <SectionTitle>{upper(t("search.messages"))}</SectionTitle>
            {messages.data!.results.map((m) => (
              <ListRow
                key={m.message_id}
                testID={`row-message-${m.message_id}`}
                minHeight={58}
                glyph={<Avatar label={(m.sender_username ?? m.sender_id).slice(0, 2)} size="sm" />}
                name={m.sender_username ?? m.sender_id}
                nameStyle={{ fontSize: 13, fontFamily: ty.rowN.fontFamily }}
                sub={m.snippet.text || m.content}
                end={
                  <Text style={ty.label}>
                    {upper(
                      new Date(m.sent_at).toLocaleDateString(activeLocale(), {
                        month: "short",
                        day: "numeric",
                      }),
                    )}
                  </Text>
                }
                onPress={() => {
                  const info = conversationKinds.get(m.conversation_id);
                  // Unknown id (e.g. a conversation we've since left): fall
                  // back to channel, the pre-fix behaviour.
                  const kind = info?.kind ?? "channel";
                  // Mirror the list rows: opening a conversation selects it
                  // (suppresses its realtime unread) and clears its count.
                  if (kind === "dm") {
                    appStore.setSelectedConversationId(m.conversation_id);
                  } else {
                    if (info?.groupId) {
                      appStore.setSelectedGroupId(info.groupId);
                    }
                    appStore.setSelectedChannelId(m.conversation_id);
                  }
                  appStore.markRead(m.conversation_id);
                  router.push({
                    pathname: "/chat/[id]",
                    params: {
                      id: m.conversation_id,
                      kind,
                      ...(info?.name ? { name: info.name } : {}),
                    },
                  });
                }}
              />
            ))}
          </View>
        ) : null}
      </Body>

      <View
        style={{
          paddingVertical: 10,
          paddingHorizontal: 14,
          borderTopWidth: 1,
          borderTopColor: semantic.hairSoft,
        }}
      >
        <Field
          testID="input-search"
          accessibilityLabel={t("search:page.title")}
          amber
          value={q}
          onChangeText={setQ}
          placeholder={t("search.placeholder")}
          icon={<Icon.search color={semantic.mute} />}
        />
      </View>
    </Screen>
  );
}
