import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Button,
  BottomAction,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { useLeaveDM } from "../../hooks/queries";
import { useAppStore } from "../../stores/appStore";

interface DmMember {
  user_id: string;
  username?: string;
  avatar_url?: string;
}

interface DmChannel {
  id: string;
  created_by: string;
  created_at: string;
  members: DmMember[];
}

export default function DMInfo() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id?: string }>();
  const channelId = id ?? null;
  const currentUser = useAppStore((s) => s.currentUser);
  const [confirmLeave, setConfirmLeave] = useState(false);

  const channel = useQuery({
    queryKey: ["dm", "channel", channelId],
    queryFn: async (): Promise<DmChannel | null> => {
      if (!channelId) {
        return null;
      }
      return await invoke<DmChannel>("get_dm_channel", {
        dmChannelId: channelId,
      });
    },
    enabled: !!channelId,
    staleTime: 1000 * 60,
  });

  const leave = useLeaveDM();

  const onLeave = () => {
    if (!confirmLeave) {
      setConfirmLeave(true);
      return;
    }
    if (!channelId) {
      return;
    }
    leave.mutate(channelId, {
      onSuccess: () => router.replace("/(tabs)/direct"),
    });
  };

  const members = channel.data?.members ?? [];

  return (
    <Screen>
      <Crumb segs={[{ label: "DIRECT" }, { label: "Info", leaf: true }]} />
      <Body>
        <SectionTitle>PARTICIPANTS</SectionTitle>
        {channel.isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 8,
            }}
          >
            Loading…
          </Text>
        ) : null}
        {members.map((m) => {
          const isMe = m.user_id === currentUser?.id;
          const handle = m.username ?? m.user_id.slice(0, 8);
          return (
            <ListRow
              key={m.user_id}
              minHeight={54}
              glyph={<Avatar label={handle.slice(0, 2)} />}
              name={`@${handle}${isMe ? " · you" : ""}`}
              nameStyle={{ fontSize: 14 }}
              onPress={
                isMe
                  ? undefined
                  : () =>
                      router.push({
                        pathname: "/user/[id]",
                        params: { id: m.user_id },
                      })
              }
              end={!isMe ? <Icon.fwd color={semantic.mute} /> : undefined}
            />
          );
        })}

        <SectionTitle>DANGER</SectionTitle>
        <View style={{ paddingHorizontal: 18 }}>
          <Button
            full
            variant="danger"
            icon={<Icon.exit color={semantic.danger} />}
            onPress={onLeave}
            disabled={leave.isPending || !channelId}
          >
            {leave.isPending
              ? "LEAVING…"
              : confirmLeave
                ? "TAP AGAIN TO CONFIRM"
                : "LEAVE CONVERSATION"}
          </Button>
          {leave.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                paddingTop: 6,
              }}
            >
              {(leave.error as Error).message || "Couldn't leave."}
            </Text>
          ) : null}
        </View>
      </Body>
      <Ctx cr="DIRECT" name="Info" />
      <BottomAction>
        <Button
          full
          variant="subtle"
          onPress={() => router.back()}
          icon={<Icon.back color={semantic.ink} />}
        >
          Back to conversation
        </Button>
      </BottomAction>
    </Screen>
  );
}
