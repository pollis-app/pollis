import { View } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Badge,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic } from "../../theme/tokens";

const GROUPS = [
  {
    name: "QUICK GROUP",
    sel: "General",
    items: [
      { n: "General", sub: "dan: frick its been a minute", unread: 2 },
      { n: "Random", sub: "meilan: meeting moved" },
    ],
  },
  {
    name: "TEST GROUP",
    items: [
      { n: "General", sub: "No new messages" },
      { n: "Random" },
    ],
  },
  {
    name: "BLUESTONE",
    items: [{ n: "General" }],
  },
];

export default function Groups() {
  const router = useRouter();
  return (
    <Screen>
      <Crumb segs={[{ label: "GROUPS", leaf: true }]} end="3" />
      <Body>
        {GROUPS.map((g) => (
          <View key={g.name}>
            <SectionTitle right={<Icon.fwd color={semantic.mute} />}>
              {g.name}
            </SectionTitle>
            {g.items.map((c) => (
              <ListRow
                key={c.n}
                selected={g.sel === c.n}
                glyph={<Icon.hash color={semantic.mute} />}
                name={c.n}
                sub={(c as { sub?: string }).sub}
                onPress={() => router.push(`/chat/${g.name}-${c.n}`)}
                end={
                  (c as { unread?: number }).unread ? (
                    <Badge>{(c as { unread?: number }).unread}</Badge>
                  ) : undefined
                }
              />
            ))}
          </View>
        ))}
      </Body>
      <BottomAction>
        <Button
          full
          icon={<Icon.plus color={semantic.ink} />}
          onPress={() => router.push("/group/quick-group")}
        >
          NEW GROUP
        </Button>
      </BottomAction>
    </Screen>
  );
}
