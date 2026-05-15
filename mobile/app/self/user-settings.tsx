import { useState } from "react";
import { View, Text } from "react-native";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Field,
  Diamond,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";

export default function UserSettings() {
  const [name, setName] = useState("dan");
  const [handle, setHandle] = useState("dan");

  return (
    <Screen>
      <Crumb segs={[{ label: "SELF" }, { label: "User settings", leaf: true }]} />
      <Body>
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: 14,
            paddingHorizontal: 18,
            paddingTop: 14,
            paddingBottom: 8,
          }}
        >
          <Avatar label="dn" size="lg" variant="amber" />
          <View style={{ flex: 1 }}>
            <Text
              style={{
                fontFamily: ty.h1.fontFamily,
                fontSize: 18,
                color: semantic.ink,
              }}
            >
              dan
            </Text>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.mute,
              }}
            >
              2 character initials · tap to change
            </Text>
          </View>
          <Chip variant="on">EDIT</Chip>
        </View>

        <SectionTitle>IDENTITY</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
          <Text style={ty.label}>DISPLAY NAME</Text>
          <Field value={name} onChangeText={setName} />
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 6 }}>
          <Text style={ty.label}>HANDLE</Text>
          <Field
            value={handle}
            onChangeText={setHandle}
            icon={
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  color: semantic.mute,
                }}
              >
                @
              </Text>
            }
            trailing={
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 10,
                  letterSpacing: 1,
                  color: semantic.accent,
                }}
              >
                AVAILABLE
              </Text>
            }
          />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            Other members can find and DM you with @handle.
          </Text>
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 6 }}>
          <Text style={ty.label}>EMAIL</Text>
          <Field
            value="dan@example.io"
            editable={false}
            icon={<Icon.mail color={semantic.mute} />}
            trailing={
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 10,
                  letterSpacing: 1,
                  color: semantic.accent,
                }}
              >
                VERIFIED
              </Text>
            }
          />
        </View>

        <SectionTitle>PRESENCE</SectionTitle>
        <ListRow
          minHeight={46}
          glyph={<Diamond size={6} />}
          name="Status"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          end={
            <>
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.accent,
                }}
              >
                Online
              </Text>
              <Icon.fwd color={semantic.mute} />
            </>
          }
        />
        <ListRow
          minHeight={46}
          glyph={<Diamond size={6} fill={false} />}
          name="Set away after"
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
                10 min
              </Text>
              <Icon.fwd color={semantic.mute} />
            </>
          }
        />
      </Body>
      <Ctx cr="SELF" name="User settings" />
    </Screen>
  );
}
