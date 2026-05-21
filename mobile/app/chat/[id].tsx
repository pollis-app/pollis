import { useMemo, useState } from "react";
import { View, Text, ScrollView, TextInput, Pressable } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { Screen, Crumb, Avatar, Ctx, CtxAct } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r } from "../../theme/tokens";
import { useMessages, type Message } from "../../hooks/queries";
import { useAppStore } from "../../stores/appStore";

function dayKey(ts: number | string): string {
  const d = new Date(typeof ts === "number" ? ts : ts);
  return d.toDateString();
}

function dayLabel(ts: number | string): string {
  const d = new Date(typeof ts === "number" ? ts : ts);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) {
    return "TODAY";
  }
  const yest = new Date(today);
  yest.setDate(today.getDate() - 1);
  if (d.toDateString() === yest.toDateString()) {
    return "YESTERDAY";
  }
  return d
    .toLocaleDateString(undefined, { month: "short", day: "numeric" })
    .toUpperCase();
}

function timeLabel(ts: number | string): string {
  const d = new Date(typeof ts === "number" ? ts : ts);
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

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
}: {
  av: string;
  amber?: boolean;
  name: string;
  time: string;
  text?: string;
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
      </View>
    </View>
  );
}

export default function TextChat() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const conversationId = id ?? null;
  const [draft, setDraft] = useState("");
  const currentUser = useAppStore((s) => s.currentUser);
  const { data: messages = [], isLoading, isError } = useMessages(conversationId);

  // Group by day boundary so we render `<Day>` headers between blocks.
  const sections = useMemo(() => {
    const out: { label: string; messages: Message[] }[] = [];
    let lastKey = "";
    for (const m of messages) {
      const k = dayKey(m.created_at);
      if (k !== lastKey) {
        out.push({ label: dayLabel(m.created_at), messages: [] });
        lastKey = k;
      }
      out[out.length - 1].messages.push(m);
    }
    return out;
  }, [messages]);

  return (
    <Screen>
      <Crumb
        segs={[{ label: "CHAT", leaf: true }]}
      />
      <ScrollView
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingVertical: 4 }}
      >
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            Loading messages…
          </Text>
        ) : null}
        {isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            Couldn't load messages.
          </Text>
        ) : null}
        {!isLoading && !isError && sections.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            No messages yet. Be the first to say something.
          </Text>
        ) : null}
        {sections.map((section, sIdx) => (
          <View key={`${section.label}-${sIdx}`}>
            <Day label={section.label} />
            {section.messages.map((m) => {
              const mine = currentUser?.id === m.sender_id;
              const name = m.sender_username || (mine ? "you" : "user");
              return (
                <Msg
                  key={m.id}
                  av={name.slice(0, 2)}
                  amber={mine}
                  name={name}
                  time={timeLabel(m.created_at)}
                  text={m.content}
                />
              );
            })}
          </View>
        ))}
      </ScrollView>

      <Ctx
        cr="CHAT"
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
              {conversationId ?? "—"}
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
