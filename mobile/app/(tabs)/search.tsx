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
  Chip,
  Button,
} from "../../components/ui";
import { SearchResultRow } from "../../components/search/SearchResultRow";
import { CorpusFooter } from "../../components/search/CorpusFooter";
import type { SearchSort } from "../../hooks/queries/useSearch";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useDMChannels,
  useSearchMessages,
  useUserGroupsWithChannels,
  useUserSearch,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { upper } from "../../i18n";

// Search filter syntax, one per line. The operators are typed literally, so
// they are not translated; only the label above them is.
const SEARCH_FILTER_EXAMPLES = [
  "from:@user",
  "in:#channel",
  "before:YYYY-MM-DD",
  "after:YYYY-MM-DD",
  "on:YYYY-MM-DD",
  "has:attachment",
  "has:link",
];

export default function Search() {
  const router = useRouter();
  const { t } = useTranslation("mobile");
  const [q, setQ] = useState("");
  const trimmed = q.trim();
  const { t: ts } = useTranslation("search");
  // null = let Rust choose (relevance for a global search); the page reports
  // what it applied, which is what the toggle shows as selected.
  const [sort, setSort] = useState<SearchSort | null>(null);
  const messages = useSearchMessages(trimmed, sort);
  const firstPage = messages.data?.pages[0];
  const messageResults = useMemo(
    () => messages.data?.pages.flatMap((p) => p.results) ?? [],
    [messages.data],
  );
  const messageTotal = firstPage?.total ?? 0;
  const activeSort: SearchSort = sort ?? firstPage?.sort ?? "relevant";
  // Message/group search below is local and runs as you type; the account
  // lookup is an exact-match DS query, so it runs only on return (#1216):
  // a lookup per keystroke would let anyone walk the directory by prefix.
  const [userQuery, setUserQuery] = useState("");
  const user = useUserSearch(userQuery);
  const userShown = !!user.data && userQuery === trimmed;
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

  // Quick-jump to settings pages — desktop's PAGE_RESULTS, for the pages the
  // mobile Self hub offers. Matched on title and subtitle, like desktop.
  const pages = useMemo(() => {
    if (trimmed.length < 2) {
      return [];
    }
    const lower = trimmed.toLowerCase();
    return [
      { id: "preferences", n: t("settings:preferences.title"), s: t("mobile:self.hub.preferencesSub"), to: "/self/preferences" as const },
      { id: "user-settings", n: t("settings:user.title"), s: t("mobile:self.hub.userSettingsSub"), to: "/self/user-settings" as const },
      { id: "security", n: t("settings:security.title"), s: t("mobile:self.hub.securitySub"), to: "/self/security" as const },
      { id: "saved", n: t("saved:page.title"), s: t("mobile:self.hub.savedSub"), to: "/self/saved" as const },
    ].filter((p) => p.n.toLowerCase().includes(lower) || p.s.toLowerCase().includes(lower));
  }, [trimmed, t]);

  const totalResults =
    (userShown ? 1 : 0) +
    filtered.groups.length +
    filtered.channels.length +
    pages.length +
    messageTotal;

  const showEmpty =
    trimmed.length >= 2 &&
    !messages.isLoading &&
    !user.isLoading &&
    totalResults === 0;

  return (
    <Screen testID="screen-search" aboveTabBar>
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
        {trimmed.length < 2 ? (
          <Text
            testID="search-filter-hint"
            style={{
              fontFamily: ty.mono.fontFamily,
              fontSize: 11,
              lineHeight: 18,
              color: semantic.mute2,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            {[t("search.filtersLabel"), ...SEARCH_FILTER_EXAMPLES].join("\n")}
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
        {showEmpty ? (
          <View testID="search-no-results-why" style={{ paddingHorizontal: 18, paddingTop: 10, gap: 4 }}>
            <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 12, color: semantic.mute }}>
              {ts("view.noResultsWhy")}
            </Text>
            {[
              "view.reasonNotIngested",
              "view.reasonBeforeJoin",
              "view.reasonNewDevice",
              "view.reasonRetention",
              "view.reasonDeleted",
            ].map((k) => (
              <Text key={k} style={{ fontFamily: ty.body.fontFamily, fontSize: 12, color: semantic.mute }}>
                {`· ${ts(k)}`}
              </Text>
            ))}
          </View>
        ) : null}

        {pages.length > 0 ? (
          <View>
            <SectionTitle>{upper(t("tabs.self"))}</SectionTitle>
            {pages.map((p) => (
              <ListRow
                key={p.id}
                testID={`row-page-${p.id}`}
                minHeight={48}
                glyph={<Icon.gear color={semantic.mute} />}
                name={p.n}
                sub={p.s}
                onPress={() => router.push(p.to)}
              />
            ))}
          </View>
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

        {userShown && user.data ? (
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

        {messageResults.length > 0 ? (
          <View>
            <SectionTitle>{upper(t("search.messages"))}</SectionTitle>
            <View
              style={{
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "flex-end",
                paddingHorizontal: 18,
                paddingBottom: 8,
                gap: 8,
              }}
            >
              <View style={{ flexDirection: "row", gap: 6 }}>
                <Chip
                  testID="search-sort-relevant"
                  variant={activeSort === "relevant" ? "on" : "default"}
                  onPress={() => setSort("relevant")}
                >
                  {ts("view.sortRelevant")}
                </Chip>
                <Chip
                  testID="search-sort-recent"
                  variant={activeSort === "recent" ? "on" : "default"}
                  onPress={() => setSort("recent")}
                >
                  {ts("view.sortRecent")}
                </Chip>
              </View>
            </View>
            {messageResults.map((m) => {
              const info = conversationKinds.get(m.conversation_id);
              const label =
                m.conversation_kind === "dm"
                  ? (m.conversation_name ?? info?.name ?? null)
                  : m.conversation_name
                    ? `#${m.conversation_name}${m.group_name ? ` · ${m.group_name}` : ""}`
                    : (info?.name ? `#${info.name}` : null);
              return (
                <SearchResultRow
                  key={m.message_id}
                  result={m}
                  conversationLabel={label}
                  onPress={() => {
                    // The row carries its kind now; the cached lists are the
                    // fallback for rows indexed before it did. Unknown (a
                    // conversation since left): channel, the old behaviour.
                    const kind = m.conversation_kind ?? info?.kind ?? "channel";
                    // Mirror the list rows: opening a conversation selects it
                    // (suppresses its realtime unread) and clears its count.
                    if (kind === "dm") {
                      appStore.setSelectedConversationId(m.conversation_id);
                    } else {
                      const groupId = m.group_id ?? info?.groupId;
                      if (groupId) {
                        appStore.setSelectedGroupId(groupId);
                      }
                      appStore.setSelectedChannelId(m.conversation_id);
                    }
                    appStore.markRead(m.conversation_id);
                    const name = m.conversation_name ?? info?.name;
                    router.push({
                      pathname: "/chat/[id]",
                      params: {
                        id: m.conversation_id,
                        kind,
                        ...(name ? { name } : {}),
                      },
                    });
                  }}
                />
              );
            })}
            {messages.hasNextPage ? (
              <View style={{ padding: 14 }}>
                <Button
                  testID="search-load-more"
                  variant="subtle"
                  full
                  disabled={messages.isFetchingNextPage}
                  onPress={() => void messages.fetchNextPage()}
                >
                  {upper(
                    messages.isFetchingNextPage
                      ? ts("view.searching")
                      : ts("view.loadMore"),
                  )}
                </Button>
              </View>
            ) : null}
          </View>
        ) : null}
        {trimmed.length >= 2 && firstPage?.corpus ? (
          <CorpusFooter corpus={firstPage.corpus} />
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
          onSubmitEditing={() => {
            if (trimmed.length >= 2) {
              setUserQuery(trimmed);
            }
          }}
          returnKeyType="search"
          placeholder={t("search.placeholder")}
          icon={<Icon.search color={semantic.mute} />}
        />
      </View>
    </Screen>
  );
}
