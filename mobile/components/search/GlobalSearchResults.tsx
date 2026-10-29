import { useMemo, useState } from "react";
import { View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Button,
  Group,
  Txt,
} from "../ui";
import { SearchResultRow } from "./SearchResultRow";
import { CorpusFooter } from "./CorpusFooter";
import type { SearchSort } from "../../hooks/queries/useSearch";
import { Icon } from "../icons";
import { semantic, space, layout } from "../../theme/tokens";
import {
  useDMChannels,
  useSearchMessages,
  useUserGroupsWithChannels,
  useUserSearch,
  markConversationRead,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { useThemeRerender } from "../theme";

// The operators are typed literally, so they are not translated.
const SEARCH_FILTERS = [
  { example: "from:@user", token: "from:@" },
  { example: "in:#channel", token: "in:#" },
  { example: "before:YYYY-MM-DD", token: "before:" },
  { example: "after:YYYY-MM-DD", token: "after:" },
  { example: "on:YYYY-MM-DD", token: "on:" },
  { example: "has:attachment", token: "has:attachment" },
  { example: "has:link", token: "has:link" },
];

/**
 * Global search results (#1202), shown in place of the Self menu while its
 * field is in use (#1550); Self owns the field and the query.
 *
 * `userQuery` is the exact-match account lookup, set on return only (#1216):
 * a lookup per keystroke would let anyone walk the directory by prefix.
 */
export function GlobalSearchResults({
  q,
  setQ,
  userQuery,
}: {
  q: string;
  setQ: React.Dispatch<React.SetStateAction<string>>;
  userQuery: string;
}) {
  useThemeRerender();
  const router = useRouter();
  const { openConversation, openGroup } = useOpenConversation();
  const { t } = useTranslation("mobile");
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
  const user = useUserSearch(userQuery);
  const userShown = !!user.data && userQuery === trimmed;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dms = [] } = useDMChannels();

  // The kind of a hit indexed before rows carried one, from the cached lists.
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

  // Client-side over the cached lists, as desktop does: there is no
  // search-everything command.
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

  // Desktop's PAGE_RESULTS, for the pages the Self hub offers.
  const pages = useMemo(() => {
    if (trimmed.length < 2) {
      return [];
    }
    const lower = trimmed.toLowerCase();
    return [
      {
        id: "preferences",
        n: t("settings:preferences.title"),
        s: t("mobile:self.hub.preferencesSub"),
        to: "/self/preferences" as const,
      },
      {
        id: "user-settings",
        n: t("settings:user.title"),
        s: t("mobile:self.hub.userSettingsSub"),
        to: "/self/user-settings" as const,
      },
      {
        id: "security",
        n: t("settings:security.title"),
        s: t("mobile:self.hub.securitySub"),
        to: "/self/security" as const,
      },
      {
        id: "security-events",
        n: t("search:pages.securityEventsName"),
        s: t("mobile:self.hub.securityEventsSub"),
        k: t("search:pages.securityEventsKeywords"),
        to: "/self/security-events" as const,
      },
      {
        id: "saved",
        n: t("saved:page.title"),
        s: t("mobile:self.hub.savedSub"),
        to: "/self/saved" as const,
      },
      {
        id: "vault",
        n: t("search:pages.vaultName"),
        s: t("mobile:vault.rowSub"),
        k: t("search:pages.vaultKeywords"),
        to: "/vault" as const,
      },
    ].filter(
      (p) =>
        p.n.toLowerCase().includes(lower) ||
        p.s.toLowerCase().includes(lower) ||
        (p.k ?? "").toLowerCase().includes(lower),
    );
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

  const addFilter = (token: string) => {
    setQ((prev) => {
      const base = prev.trimEnd();
      return base ? `${base} ${token}` : token;
    });
  };

  return (
    <Body
      testID="search-results"
      contentContainerStyle={{ paddingHorizontal: space.xxl, gap: space.xs }}
    >
      {trimmed.length < 2 ? (
        <Txt
          variant="secondary"
          style={{ paddingTop: space.sm }}
        >
          {t("search.hint")}
        </Txt>
      ) : null}
      {trimmed.length < 2 ? (
        <View>
          <SectionTitle testID="search-filter-hint" style={sectionStyle}>
            {t("search.filtersLabel")}
          </SectionTitle>
          <View
            style={{
              flexDirection: "row",
              flexWrap: "wrap",
              columnGap: space.sm,
            }}
          >
            {SEARCH_FILTERS.map((f) => (
              <Chip
                key={f.example}
                testID={`search-filter-${f.token.replace(/[^a-z]/g, "")}`}
                variant="outline"
                onPress={() => addFilter(f.token)}
              >
                {f.example}
              </Chip>
            ))}
          </View>
        </View>
      ) : null}
      {showEmpty ? (
        <Txt
          variant="body"
          style={{ paddingTop: space.lg }}
        >
          {t("search:panel.noMatches")}
        </Txt>
      ) : null}
      {showEmpty ? (
        <View
          testID="search-no-results-why"
          style={{
            paddingTop: space.sm,
            gap: space.xs,
          }}
        >
          <Txt variant="secondary">{ts("view.noResultsWhy")}</Txt>
          {[
            "view.reasonNotIngested",
            "view.reasonBeforeJoin",
            "view.reasonNewDevice",
            "view.reasonRetention",
            "view.reasonDeleted",
          ].map((k) => (
            <Txt key={k} variant="secondary">
              {`· ${ts(k)}`}
            </Txt>
          ))}
        </View>
      ) : null}

      {pages.length > 0 ? (
        <View>
          <SectionTitle style={sectionStyle}>{t("tabs.self")}</SectionTitle>
          <Group>
            {pages.map((p) => (
              <ListRow
                key={p.id}
                testID={`row-page-${p.id}`}
                glyph={
                  p.id === "vault" ? (
                    <Icon.vault size={20} color={semantic.dim} />
                  ) : (
                    <Icon.gear size={20} color={semantic.dim} />
                  )
                }
                name={p.n}
                sub={p.s}
                chevron
                onPress={() => router.push(p.to)}
              />
            ))}
          </Group>
        </View>
      ) : null}

      {filtered.groups.length > 0 ? (
        <View>
          <SectionTitle style={sectionStyle}>{t("tabs.groups")}</SectionTitle>
          <Group>
            {filtered.groups.map((g) => (
              <ListRow
                key={g.id}
                testID={`row-group-${g.id}`}
                glyph={<Avatar label={g.name} size={30} shape="rounded" />}
                name={g.name}
                chevron
                onPress={() => openGroup(g.id)}
              />
            ))}
          </Group>
        </View>
      ) : null}

      {filtered.channels.length > 0 ? (
        <View>
          <SectionTitle style={sectionStyle}>
            {t("search.channels")}
          </SectionTitle>
          <Group>
            {filtered.channels.map((c) => (
              <ListRow
                key={c.id}
                testID={`row-channel-${c.id}`}
                glyph={<Icon.hash size={20} color={semantic.dim} />}
                name={c.name}
                sub={c.groupName}
                onPress={() => {
                  appStore.setSelectedGroupId(c.groupId);
                  appStore.setSelectedChannelId(c.id);
                  markConversationRead(c.id);
                  openConversation({
                    id: c.id,
                    kind: "channel",
                    name: c.name,
                    groupId: c.groupId,
                  });
                }}
              />
            ))}
          </Group>
        </View>
      ) : null}

      {userShown && user.data ? (
        <View>
          <SectionTitle style={sectionStyle}>{t("tabs.direct")}</SectionTitle>
          <Group>
            <ListRow
              testID={`row-user-${user.data.id}`}
              glyph={
                <Avatar
                  label={user.data.username || undefined}
                  size={layout.touchMin - 8}
                />
              }
              name={`@${user.data.username}`}
              sub={user.data.preferred_name || user.data.email || undefined}
              chevron
              onPress={() =>
                router.push({
                  pathname: "/user/[id]",
                  params: { id: user.data!.id },
                })
              }
            />
          </Group>
        </View>
      ) : null}

      {messageResults.length > 0 ? (
        <View>
          <SectionTitle style={sectionStyle}>
            {t("search.messages")}
          </SectionTitle>
          <View
            style={{
              flexDirection: "row",
              flexWrap: "wrap",
              alignItems: "center",
              justifyContent: "space-between",
              columnGap: space.sm,
            }}
          >
            <Txt
              variant="meta"
              testID="search-about-results"
              accessibilityLiveRegion="polite"
            >
              {ts("view.aboutResults", { count: messageTotal })}
            </Txt>
            <View
              style={{ flexDirection: "row", gap: space.sm }}
              accessibilityRole="radiogroup"
            >
              <Chip
                testID="search-sort-relevant"
                selected={activeSort === "relevant"}
                variant="outline"
                onPress={() => setSort("relevant")}
              >
                {ts("view.sortRelevant")}
              </Chip>
              <Chip
                testID="search-sort-recent"
                selected={activeSort === "recent"}
                variant="outline"
                onPress={() => setSort("recent")}
              >
                {ts("view.sortRecent")}
              </Chip>
            </View>
          </View>
          <Group style={{ marginTop: space.xs }}>
            {messageResults.map((m) => {
              const info = conversationKinds.get(m.conversation_id);
              const label =
                m.conversation_kind === "dm"
                  ? (m.conversation_name ?? info?.name ?? null)
                  : m.conversation_name
                    ? `#${m.conversation_name}${m.group_name ? ` · ${m.group_name}` : ""}`
                    : info?.name
                      ? `#${info.name}`
                      : null;
              return (
                <SearchResultRow
                  key={m.message_id}
                  result={m}
                  isSelf={m.sender_id === appStore.currentUser?.id}
                  conversationLabel={label}
                  onPress={() => {
                    // Unknown (a conversation since left) falls back to channel.
                    const kind =
                      m.conversation_kind ?? info?.kind ?? "channel";
                    // Selecting it suppresses its realtime unread, as the
                    // list rows do.
                    if (kind === "dm") {
                      appStore.setSelectedConversationId(m.conversation_id);
                    } else {
                      const groupId = m.group_id ?? info?.groupId;
                      if (groupId) {
                        appStore.setSelectedGroupId(groupId);
                      }
                      appStore.setSelectedChannelId(m.conversation_id);
                    }
                    markConversationRead(m.conversation_id);
                    const name = m.conversation_name ?? info?.name;
                    openConversation({
                      id: m.conversation_id,
                      kind,
                      name: name ?? undefined,
                      groupId: m.group_id ?? info?.groupId ?? undefined,
                    });
                  }}
                />
              );
            })}
          </Group>
          {messages.hasNextPage ? (
            <View style={{ paddingTop: space.lg }}>
              <Button
                testID="search-load-more"
                variant="secondary"
                full
                disabled={messages.isFetchingNextPage}
                onPress={() => void messages.fetchNextPage()}
              >
                {messages.isFetchingNextPage
                  ? ts("view.searching")
                  : ts("view.loadMore")}
              </Button>
            </View>
          ) : null}
        </View>
      ) : null}
      {trimmed.length >= 2 && firstPage?.corpus ? (
        <CorpusFooter corpus={firstPage.corpus} />
      ) : null}
    </Body>
  );
}

const sectionStyle = { paddingHorizontal: 0, paddingTop: space.xxl };
