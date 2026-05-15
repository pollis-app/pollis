import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  ListRow,
  Avatar,
  Badge,
  Diamond,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";

const DMS = [
  { h: "brian", s: "recently onlineman", t: "21:34", unread: 2, v: true },
  { h: "meilan.solly", s: "hello", t: "MAY 5", v: true },
  { h: "c8", s: "verified key 0x4a…", t: "MAY 2", v: true },
  { h: "j0n", s: "unverified · tap to verify", t: "APR", v: false },
  { h: "rune.4", s: "see you tomorrow", t: "APR", v: true },
];

export default function Direct() {
  const router = useRouter();
  return (
    <Screen>
      <Crumb segs={[{ label: "DIRECT", leaf: true }]} end="5" />
      <Body>
        {DMS.map((d) => (
          <ListRow
            key={d.h}
            minHeight={64}
            onPress={() => router.push(`/chat/dm-${d.h}`)}
            glyph={
              <Avatar
                label={d.h.slice(0, 2)}
                style={{
                  borderColor: d.v ? semantic.hairStrong : semantic.mute2,
                }}
              />
            }
            name={
              <View
                style={{ flexDirection: "row", alignItems: "center", gap: 6 }}
              >
                <Text
                  style={{
                    fontFamily: ty.rowN.fontFamily,
                    fontSize: 15,
                    color: semantic.ink,
                  }}
                >
                  @{d.h}
                </Text>
                {d.v && <Diamond size={6} />}
              </View>
            }
            sub={d.s}
            end={
              <View style={{ alignItems: "flex-end", gap: 4 }}>
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 10,
                    color: semantic.mute,
                  }}
                >
                  {d.t}
                </Text>
                {d.unread ? <Badge>{d.unread}</Badge> : null}
              </View>
            }
          />
        ))}
      </Body>
      <BottomAction>
        <Button full icon={<Icon.plus color={semantic.ink} />}>
          NEW DIRECT MESSAGE
        </Button>
      </BottomAction>
    </Screen>
  );
}
