import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Badge,
  Chip,
  Ctx,
  CtxAct,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";

export default function GroupDetail() {
  const router = useRouter();
  // Built per-render so glyph colors track the live accent.
  const ADMIN = [
    { g: <Icon.people color={semantic.mute} />, n: "Members", s: "3 · 1 admin" },
    { g: <Icon.at color={semantic.mute} />, n: "Invite a member" },
    { g: <Icon.plus color={semantic.mute} />, n: "New channel" },
    { g: <Icon.inbox color={semantic.mute} />, n: "Join requests", badge: "2" },
    { g: <Icon.edit color={semantic.mute} />, n: "Rename group" },
  ];
  return (
    <Screen>
      <Crumb
        segs={[{ label: "GROUPS" }, { label: "Quick Group", leaf: true }]}
        end="3 MEMBERS"
      />
      <Body>
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: 10,
            paddingHorizontal: 18,
            paddingTop: 8,
            paddingBottom: 16,
          }}
        >
          <View style={{ flexDirection: "row" }}>
            <Avatar label="dn" size="sm" variant="amber" style={{ marginRight: -8 }} />
            <Avatar label="br" size="sm" style={{ marginRight: -8 }} />
            <Avatar label="me" size="sm" />
          </View>
          <Text
            style={{
              flex: 1,
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
            }}
          >
            dan, brian, meilan.solly
          </Text>
          <Chip>View all</Chip>
        </View>

        <SectionTitle>TEXT CHANNELS</SectionTitle>
        <ListRow
          selected
          minHeight={54}
          glyph={<Icon.hash color={semantic.accent} />}
          name="General"
          sub="dan: frick its been a minute"
          onPress={() => router.push("/chat/quick-general")}
          end={<Badge>2</Badge>}
        />
        <ListRow
          minHeight={54}
          glyph={<Icon.hash color={semantic.mute} />}
          name="Random"
          sub="No new messages"
          onPress={() => router.push("/chat/quick-random")}
        />

        <SectionTitle>ADMIN</SectionTitle>
        {ADMIN.map((rw, i) => (
          <ListRow
            key={i}
            minHeight={48}
            glyph={rw.g}
            name={rw.n}
            nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
            end={
              <>
                {rw.badge ? <Badge>{rw.badge}</Badge> : null}
                <Icon.fwd color={semantic.mute} />
              </>
            }
          />
        ))}

        <SectionTitle>DANGER</SectionTitle>
        <ListRow
          minHeight={48}
          glyph={<Icon.exit color={semantic.danger} />}
          name="Leave group"
          nameStyle={{
            fontSize: 14,
            fontFamily: ty.body.fontFamily,
            color: semantic.danger,
          }}
        />
      </Body>

      <Ctx
        cr="GROUPS"
        name="Quick Group"
        actions={<CtxAct icon={<Icon.kebab color={semantic.ink2} />} />}
      />
    </Screen>
  );
}
