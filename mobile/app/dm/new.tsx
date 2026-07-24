import { useState } from "react";
import { View, Text, Pressable } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  Field,
  ListRow,
  Avatar,
  Ctx,
  CtxAct,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useUserSearch, useCreateDM } from "../../hooks/queries";

export default function NewDM() {
  const router = useRouter();
  const [query, setQuery] = useState("");
  const search = useUserSearch(query);
  const createDM = useCreateDM();

  const onStartDM = (userId: string) => {
    createDM.mutate(
      { memberIds: [userId] },
      {
        onSuccess: (channel) => {
          router.replace({
            pathname: "/chat/[id]",
            params: { id: channel.id, kind: "dm" },
          });
        },
      },
    );
  };

  const found = search.data;
  const showEmpty =
    !search.isLoading && !search.isError && query.trim().length >= 2 && !found;

  return (
    <Screen testID="screen-dm-new" centered>
      <Crumb
        segs={[{ label: "DIRECT" }, { label: "New", leaf: true }]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 8 }}>
          <Text style={ty.label}>USERNAME OR EMAIL</Text>
          <Field
            amber
            value={query}
            onChangeText={setQuery}
            testID="input-user-search"
            accessibilityLabel="Username or email"
            icon={<Icon.search color={semantic.mute} />}
          />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            Type at least two characters. Exact match only — Pollis doesn't
            broadcast partial matches.
          </Text>
        </View>

        <View style={{ paddingTop: 8 }}>
          {search.isLoading && query.trim().length >= 2 ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.mute,
                paddingHorizontal: 18,
                paddingVertical: 12,
              }}
            >
              Searching…
            </Text>
          ) : null}
          {search.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.danger,
                paddingHorizontal: 18,
                paddingVertical: 12,
              }}
            >
              Search failed.
            </Text>
          ) : null}
          {showEmpty ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.mute,
                paddingHorizontal: 18,
                paddingVertical: 12,
              }}
            >
              No user found.
            </Text>
          ) : null}
          {found ? (
            <ListRow
              testID={`row-user-${found.id}`}
              accessibilityLabel={`Start DM with @${found.username}`}
              minHeight={64}
              glyph={
                <Avatar
                  label={(found.username || found.email || "us").slice(0, 2)}
                />
              }
              name={
                <Text
                  style={{
                    fontFamily: ty.rowN.fontFamily,
                    fontSize: 15,
                    color: semantic.ink,
                  }}
                >
                  @{found.username}
                </Text>
              }
              sub={
                found.preferred_name || found.email || undefined
              }
              onPress={() => onStartDM(found.id)}
              end={<Icon.fwd color={semantic.mute} />}
            />
          ) : null}
          {createDM.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                paddingHorizontal: 18,
                paddingTop: 8,
              }}
            >
              {(createDM.error as Error).message ||
                "Couldn't open a DM with this user."}
            </Text>
          ) : null}
        </View>
      </Body>
      <Ctx
        cr="DIRECT"
        name={
          <Pressable onPress={() => router.back()}>
            <Text
              style={{
                fontFamily: ty.rowN.fontFamily,
                fontSize: 13,
                color: semantic.ink,
              }}
            >
              ← Back to inbox
            </Text>
          </Pressable>
        }
        actions={
          <CtxAct
            icon={<Icon.exit color={semantic.ink2} />}
            onPress={() => router.back()}
          />
        }
      />
    </Screen>
  );
}
