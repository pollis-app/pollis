import { useMemo, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
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
  useSearchMessages,
  useUserGroupsWithChannels,
  useUserSearch,
} from "../../hooks/queries";

export default function Search() {
  const router = useRouter();
  const [q, setQ] = useState("");
  const trimmed = q.trim();
  const messages = useSearchMessages(trimmed);
  const user = useUserSearch(trimmed);
  const { data: groups = [] } = useUserGroupsWithChannels();

  // Client-side filter of cached groups/channels. The Rust DB doesn't
  // expose a single "search everything" command — desktop also stitches
  // these on the frontend.
  const filtered = useMemo(() => {
    if (trimmed.length < 2) {
      return { groups: [], channels: [] as { id: string; name: string; groupName: string }[] };
    }
    const lower = trimmed.toLowerCase();
    const matchingGroups = groups.filter((g) =>
      g.name.toLowerCase().includes(lower),
    );
    const matchingChannels: { id: string; name: string; groupName: string }[] = [];
    for (const g of groups) {
      for (const c of g.channels) {
        if (c.name.toLowerCase().includes(lower)) {
          matchingChannels.push({ id: c.id, name: c.name, groupName: g.name });
        }
      }
    }
    return { groups: matchingGroups, channels: matchingChannels };
  }, [groups, trimmed]);

  const totalResults =
    (user.data ? 1 : 0) +
    filtered.groups.length +
    filtered.channels.length +
    (messages.data?.length ?? 0);

  const showEmpty =
    trimmed.length >= 2 &&
    !messages.isLoading &&
    !user.isLoading &&
    totalResults === 0;

  return (
    <Screen>
      <Crumb
        segs={[{ label: "SEARCH", leaf: true }]}
        end={trimmed.length >= 2 ? `${totalResults} RESULTS` : "TYPE…"}
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
            Type at least two characters to search groups, channels, people,
            and messages.
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
            Nothing matched.
          </Text>
        ) : null}

        {filtered.groups.length > 0 ? (
          <View>
            <SectionTitle>GROUPS</SectionTitle>
            {filtered.groups.map((g) => (
              <ListRow
                key={g.id}
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
            <SectionTitle>CHANNELS</SectionTitle>
            {filtered.channels.map((c) => (
              <ListRow
                key={c.id}
                minHeight={48}
                glyph={<Icon.hash color={semantic.mute} />}
                name={c.name}
                sub={c.groupName}
                onPress={() =>
                  router.push({
                    pathname: "/chat/[id]",
                    params: { id: c.id, kind: "channel" },
                  })
                }
              />
            ))}
          </View>
        ) : null}

        {user.data ? (
          <View>
            <SectionTitle>DIRECT</SectionTitle>
            <ListRow
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

        {(messages.data?.length ?? 0) > 0 ? (
          <View>
            <SectionTitle>MESSAGES</SectionTitle>
            {messages.data!.map((m) => (
              <ListRow
                key={m.message_id}
                minHeight={58}
                glyph={<Avatar label={m.sender_id.slice(0, 2)} size="sm" />}
                name={m.sender_id}
                nameStyle={{ fontSize: 13, fontFamily: ty.rowN.fontFamily }}
                sub={m.snippet || m.content}
                end={
                  <Text style={ty.label}>
                    {new Date(m.sent_at)
                      .toLocaleDateString(undefined, {
                        month: "short",
                        day: "numeric",
                      })
                      .toUpperCase()}
                  </Text>
                }
                onPress={() =>
                  router.push({
                    pathname: "/chat/[id]",
                    params: { id: m.conversation_id, kind: "channel" },
                  })
                }
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
          amber
          value={q}
          onChangeText={setQ}
          placeholder="Search everything…"
          icon={<Icon.search color={semantic.mute} />}
        />
      </View>
    </Screen>
  );
}
