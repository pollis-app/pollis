import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Card,
  Chip,
  Toggle,
  Button,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts } from "../../theme/tokens";

const DEVICES = [
  { n: "iPhone · this device", s: "paired May 2 · last seen now", cur: true },
  { n: "MacBook Pro", s: "paired Apr 18 · last seen 14:02" },
  { n: "iPad", s: "paired Mar 4 · idle for 22 days" },
];

export default function Security() {
  const router = useRouter();
  const [hideNotif, setHideNotif] = useState(true);

  return (
    <Screen>
      <Crumb segs={[{ label: "SELF" }, { label: "Security", leaf: true }]} />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12 }}>
          <Card>
            <Text style={[ty.label, { marginBottom: 8 }]}>YOUR PUBLIC KEY</Text>
            <Text
              style={{
                fontFamily: fonts.mono400,
                fontSize: 13,
                color: semantic.accent,
                marginBottom: 10,
              }}
            >
              ed25519 · 0x4a2c 8f17 d3b9 ec40
            </Text>
            <View style={{ flexDirection: "row", gap: 8 }}>
              <Chip>Copy</Chip>
              <Chip>Show QR</Chip>
              <Chip variant="on" style={{ marginLeft: "auto" }}>
                ◆ Verified
              </Chip>
            </View>
          </Card>
        </View>

        <SectionTitle>DEVICES</SectionTitle>
        {DEVICES.map((d, i) => (
          <ListRow
            key={i}
            minHeight={54}
            glyph={<Icon.device color={semantic.mute} />}
            name={d.n}
            nameStyle={{ fontSize: 14 }}
            sub={d.s}
            end={
              d.cur ? (
                <Chip variant="on">CURRENT</Chip>
              ) : (
                <Chip>Revoke</Chip>
              )
            }
          />
        ))}

        <SectionTitle>RECOVERY</SectionTitle>
        <ListRow
          minHeight={50}
          glyph={<Icon.key color={semantic.mute} />}
          name="Recovery phrase"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub="12 words · last viewed 14d ago"
          end={<Icon.fwd color={semantic.mute} />}
        />
        <ListRow
          minHeight={50}
          glyph={<Icon.lock color={semantic.mute} />}
          name="Device PIN"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub="4 digits · biometrics on"
          end={<Icon.fwd color={semantic.mute} />}
        />

        <SectionTitle>SESSION</SectionTitle>
        <ListRow
          minHeight={46}
          name="Auto-lock after"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          end={
            <>
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.ink2,
                }}
              >
                5 min
              </Text>
              <Icon.fwd color={semantic.mute} />
            </>
          }
        />
        <ListRow
          minHeight={46}
          name="Hide notification content when locked"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          end={
            <Toggle on={hideNotif} onPress={() => setHideNotif((v) => !v)} />
          }
        />

        <View style={{ paddingHorizontal: 18, paddingTop: 14 }}>
          <Button
            full
            variant="danger"
            icon={<Icon.exit color={semantic.danger} />}
            onPress={() => router.replace("/(auth)/email")}
          >
            SIGN OUT EVERYWHERE
          </Button>
        </View>
      </Body>
      <Ctx cr="SELF" name="Security" />
    </Screen>
  );
}
