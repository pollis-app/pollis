import { useState } from "react";
import { View, Text } from "react-native";
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

function Hi({ children }: { children: string }) {
  return (
    <Text
      style={{
        backgroundColor: semantic.accentSoft,
        color: semantic.ink,
      }}
    >
      {children}
    </Text>
  );
}

export default function Search() {
  const [q, setQ] = useState("quick");
  return (
    <Screen>
      <Crumb segs={[{ label: "SEARCH", leaf: true }]} end="12 RESULTS" />
      <Body>
        <SectionTitle>GROUPS</SectionTitle>
        <ListRow
          minHeight={46}
          glyph={<Icon.diamond size={14} color={semantic.mute} />}
          name={
            <Text style={{ fontFamily: ty.rowN.fontFamily, fontSize: 14, color: semantic.ink }}>
              <Hi>Quick</Hi> Group
            </Text>
          }
        />

        <SectionTitle>CHANNELS</SectionTitle>
        <ListRow
          selected
          minHeight={48}
          glyph={<Icon.hash color={semantic.accent} />}
          name="General"
          sub={
            <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 12, color: semantic.mute }}>
              <Hi>Quick</Hi> Group
            </Text>
          }
          end={<Text style={ty.label}>↵</Text>}
        />
        <ListRow
          minHeight={48}
          glyph={<Icon.hash color={semantic.mute} />}
          name="quick-notes"
          sub="Test Group"
        />

        <SectionTitle>DIRECT</SectionTitle>
        <ListRow
          minHeight={48}
          glyph={<Avatar label="qb" size="sm" />}
          name={
            <Text style={{ fontFamily: ty.rowN.fontFamily, fontSize: 14, color: semantic.ink }}>
              @<Hi>quick</Hi>brian
            </Text>
          }
          sub="last: APR 22"
        />

        <SectionTitle>MESSAGES</SectionTitle>
        <ListRow
          minHeight={58}
          glyph={<Avatar label="me" size="sm" />}
          name={
            <Text style={{ fontFamily: ty.rowN.fontFamily, fontSize: 13, color: semantic.ink }}>
              meilan{" "}
              <Text style={{ color: semantic.mute, fontFamily: ty.body.fontFamily }}>
                · Test / General
              </Text>
            </Text>
          }
          sub={
            <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 12, color: semantic.mute }}>
              that was a <Hi>quick</Hi> response
            </Text>
          }
          end={<Text style={ty.label}>MAY 3</Text>}
        />

        <SectionTitle>SETTINGS</SectionTitle>
        <ListRow
          minHeight={44}
          glyph={<Icon.bell color={semantic.mute} />}
          name={
            <Text style={{ fontFamily: ty.rowN.fontFamily, fontSize: 14, color: semantic.ink }}>
              <Hi>Quick</Hi> reply notifications
            </Text>
          }
        />
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
          trailing={<Text style={ty.label}>ESC</Text>}
        />
      </View>
    </Screen>
  );
}
