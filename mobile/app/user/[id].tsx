import { View, Text, Pressable } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  Avatar,
  Card,
  Chip,
  Button,
  Ctx,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import {
  useSafetyNumber,
  useSetContactVerified,
  useCreateDM,
  useBlockedUsers,
  useBlockUser,
  useUnblockUser,
} from "../../hooks/queries";
import { useAppStore } from "../../stores/appStore";

interface RawProfile {
  id: string;
  username?: string;
  preferred_name?: string;
  avatar_url?: string;
}

export default function UserProfile() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const peerId = id ?? null;
  const currentUser = useAppStore((s) => s.currentUser);
  const isSelf = currentUser?.id === peerId;

  const profile = useQuery({
    queryKey: ["user", "profile", peerId],
    queryFn: async (): Promise<RawProfile | null> => {
      if (!peerId) {
        return null;
      }
      return await invoke<RawProfile | null>("get_user_profile", {
        userId: peerId,
      });
    },
    enabled: !!peerId,
    staleTime: 1000 * 60,
  });

  const safety = useSafetyNumber(isSelf ? null : peerId);
  const setVerified = useSetContactVerified();
  const createDM = useCreateDM();

  const blockedUsers = useBlockedUsers();
  const isBlocked =
    !!peerId && (blockedUsers.data ?? []).some((b) => b.blocked_id === peerId);
  const block = useBlockUser();
  const unblock = useUnblockUser();

  const handle = profile.data?.username ?? peerId ?? "user";
  const display = profile.data?.preferred_name || handle;
  const avatarLabel = handle.slice(0, 2);

  const onMessage = () => {
    if (!peerId) {
      return;
    }
    createDM.mutate(
      { memberIds: [peerId] },
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

  const onToggleVerified = () => {
    if (!peerId || !safety.data) {
      return;
    }
    setVerified.mutate({
      peerUserId: peerId,
      verified: safety.data.verification !== "verified",
    });
  };

  const onToggleBlock = () => {
    if (!peerId) {
      return;
    }
    if (isBlocked) {
      unblock.mutate(peerId);
    } else {
      block.mutate(peerId);
    }
  };

  return (
    <Screen>
      <Crumb segs={[{ label: "USER" }, { label: display, leaf: true }]} />
      <Body>
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: 14,
            paddingHorizontal: 18,
            paddingTop: 14,
            paddingBottom: 16,
          }}
        >
          <Avatar label={avatarLabel} size="lg" />
          <View style={{ flex: 1 }}>
            <Text
              style={{
                fontFamily: ty.h1.fontFamily,
                fontSize: 20,
                color: semantic.ink,
              }}
            >
              {display}
            </Text>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.mute,
              }}
            >
              @{handle}
            </Text>
          </View>
        </View>

        {isSelf ? (
          <View style={{ paddingHorizontal: 18, paddingTop: 4 }}>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.mute,
              }}
            >
              This is you. Edit your handle and display name in{" "}
              <Text
                onPress={() => router.push("/self/user-settings")}
                style={{ color: semantic.accent }}
              >
                Self → User settings
              </Text>
              .
            </Text>
          </View>
        ) : (
          <View>
            <SectionTitle>SAFETY NUMBER</SectionTitle>
            <View style={{ paddingHorizontal: 18 }}>
              {safety.isLoading ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 13,
                    color: semantic.mute,
                  }}
                >
                  Computing…
                </Text>
              ) : safety.isError ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 13,
                    color: semantic.danger,
                  }}
                >
                  {(safety.error as Error).message ||
                    "Couldn't fetch safety number."}
                </Text>
              ) : safety.data ? (
                <Card
                  style={{
                    borderColor:
                      safety.data.verification === "verified"
                        ? semantic.accent
                        : safety.data.verification === "changed"
                          ? semantic.danger
                          : semantic.hair,
                  }}
                >
                  <Text
                    selectable
                    style={{
                      fontFamily: fonts.mono400,
                      fontSize: 13,
                      lineHeight: 22,
                      color: semantic.ink,
                      letterSpacing: 0.4,
                    }}
                  >
                    {safety.data.combined}
                  </Text>
                  <View
                    style={{
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                      marginTop: 12,
                    }}
                  >
                    <Pressable onPress={onToggleVerified}>
                      <Chip
                        variant={
                          safety.data.verification === "verified" ? "on" : "default"
                        }
                      >
                        {setVerified.isPending
                          ? "…"
                          : safety.data.verification === "verified"
                            ? "◆ Verified"
                            : "Mark verified"}
                      </Chip>
                    </Pressable>
                    {safety.data.verification === "changed" ? (
                      <Text
                        style={{
                          fontFamily: ty.body.fontFamily,
                          fontSize: 11,
                          color: semantic.danger,
                          flex: 1,
                        }}
                      >
                        Key changed since you last verified — re-verify in
                        person.
                      </Text>
                    ) : null}
                  </View>
                </Card>
              ) : null}
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 11,
                  color: semantic.mute,
                  paddingTop: 10,
                  lineHeight: 16,
                }}
              >
                Compare these digits in person or over an out-of-band
                channel. Matching numbers prove you're talking to the same
                key the server published for this user.
              </Text>
            </View>

            <SectionTitle>SAFETY ACTIONS</SectionTitle>
            <View style={{ paddingHorizontal: 18 }}>
              <Button
                full
                variant={isBlocked ? "default" : "danger"}
                icon={
                  <Icon.exit
                    color={isBlocked ? semantic.ink : semantic.danger}
                  />
                }
                onPress={onToggleBlock}
                disabled={block.isPending || unblock.isPending}
              >
                {block.isPending || unblock.isPending
                  ? "WORKING…"
                  : isBlocked
                    ? "UNBLOCK USER"
                    : "BLOCK USER"}
              </Button>
            </View>
          </View>
        )}
      </Body>
      <Ctx cr="USER" name={display} />
      {!isSelf ? (
        <BottomAction>
          <Button
            full
            variant="primary"
            onPress={onMessage}
            disabled={createDM.isPending}
            iconRight={<Icon.send color="#0a0907" />}
          >
            {createDM.isPending ? "OPENING…" : "MESSAGE"}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}
