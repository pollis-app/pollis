import { useState } from "react";
import { View, Text, ScrollView, TextInput, Pressable } from "react-native";
import {
  Screen,
  Crumb,
  Avatar,
  Ctx,
  CtxAct,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r } from "../../theme/tokens";

function Day({ label }: { label: string }) {
  return (
    <View
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
        paddingHorizontal: 18,
        paddingTop: 14,
        paddingBottom: 8,
      }}
    >
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }} />
      <Text style={[ty.label, { letterSpacing: 2.2 }]}>{label}</Text>
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }} />
    </View>
  );
}

function Msg({
  av,
  amber,
  name,
  time,
  text,
  image,
}: {
  av: string;
  amber?: boolean;
  name: string;
  time: string;
  text?: string;
  image?: string;
}) {
  return (
    <View
      style={{
        flexDirection: "row",
        gap: 12,
        paddingHorizontal: 18,
        paddingVertical: 8,
      }}
    >
      <Avatar label={av} variant={amber ? "amber" : "default"} />
      <View style={{ flex: 1 }}>
        <View style={{ flexDirection: "row", alignItems: "baseline", gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.h1.fontFamily,
              fontSize: 14,
              color: semantic.ink,
            }}
          >
            {name}
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            {time}
          </Text>
        </View>
        {text ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 14,
              lineHeight: 20,
              color: semantic.ink,
              marginTop: 2,
            }}
          >
            {text}
          </Text>
        ) : null}
        {image ? (
          <View
            style={{
              width: 180,
              height: 120,
              marginTop: 4,
              borderWidth: 1,
              borderColor: semantic.hair,
              borderRadius: r.sm,
              backgroundColor: semantic.cardBg,
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            <Text style={[ty.label, { letterSpacing: 1.8 }]}>{image}</Text>
          </View>
        ) : null}
      </View>
    </View>
  );
}

export default function TextChat() {
  const [draft, setDraft] = useState("");
  return (
    <Screen>
      <Crumb
        segs={[
          { label: "GROUPS" },
          { label: "Quick Group" },
          { label: "# General", leaf: true },
        ]}
      />
      <ScrollView style={{ flex: 1 }} contentContainerStyle={{ paddingVertical: 4 }}>
        <Day label="MAY 5" />
        <Msg av="me" name="meilan.solly" time="21:34" text="hello" />
        <Msg av="dn" amber name="dan" time="21:34" text="test" />
        <Msg av="me" name="meilan.solly" time="21:36" image="NOODLES.JPG" />
        <Day label="TODAY" />
        <Msg av="br" name="brian" time="11:11" text="recently onlineman" />
        <Msg av="dn" amber name="dan" time="00:00" text="frick its been a minute" />
      </ScrollView>

      <Ctx
        cr="QUICK GROUP"
        name={
          <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
            <Icon.hash color={semantic.ink} />
            <Text
              style={{
                fontFamily: ty.rowN.fontFamily,
                fontSize: 15,
                color: semantic.ink,
              }}
            >
              General
            </Text>
          </View>
        }
        actions={
          <>
            <CtxAct icon={<Icon.people color={semantic.ink2} />} />
            <CtxAct icon={<Icon.kebab color={semantic.ink2} />} />
          </>
        }
      />
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          gap: 10,
          paddingVertical: 10,
          paddingHorizontal: 12,
          borderTopWidth: 1,
          borderTopColor: semantic.hairSoft,
        }}
      >
        <Pressable
          style={{
            width: 38,
            height: 38,
            alignItems: "center",
            justifyContent: "center",
            borderWidth: 1,
            borderColor: semantic.hairStrong,
            borderRadius: r.sm,
          }}
        >
          <Icon.plus color={semantic.ink} />
        </Pressable>
        <TextInput
          value={draft}
          onChangeText={setDraft}
          placeholder="Type a message…"
          placeholderTextColor={semantic.mute}
          style={{
            flex: 1,
            borderWidth: 1,
            borderColor: semantic.hairStrong,
            borderRadius: r.sm,
            paddingVertical: 10,
            paddingHorizontal: 12,
            fontFamily: ty.body.fontFamily,
            fontSize: 14,
            color: semantic.ink,
            backgroundColor: semantic.fieldBg,
          }}
        />
        <Pressable
          onPress={() => setDraft("")}
          style={{
            width: 38,
            height: 38,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: semantic.accent,
            borderRadius: r.sm,
          }}
        >
          <Icon.send color="#0a0907" />
        </Pressable>
      </View>
    </Screen>
  );
}
