import { View, Text, Pressable } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { Trans, useTranslation } from "react-i18next";
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
import { upper } from "../../i18n";
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
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

interface RawProfile {
  id: string;
  username?: string;
  preferred_name?: string;
  avatar_url?: string;
}

function UserProfile() {
  const { t } = useTranslation("dms");
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const peerId = id ?? null;
  const currentUser = appStore.currentUser;
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

  const handle =
    profile.data?.username ?? peerId ?? t("mobile:user.fallbackHandle");
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
    <Screen testID="screen-user">
      <Crumb
        segs={[
          { label: upper(t("mobile:user.title")) },
          { label: display, leaf: true },
        ]}
      />
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
              <Trans
                t={t}
                i18nKey="mobile:user.selfNote"
                components={{
                  link: (
                    <Text
                      onPress={() => router.push("/self/user-settings")}
                      style={{ color: semantic.accent }}
                    />
                  ),
                }}
              />
            </Text>
          </View>
        ) : (
          <View>
            <SectionTitle>{upper(t("profile.safetyNumber"))}</SectionTitle>
            <View style={{ paddingHorizontal: 18 }}>
              {safety.isLoading ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 13,
                    color: semantic.mute,
                  }}
                >
                  {t("mobile:user.computing")}
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
                    t("mobile:user.safetyNumberFailed")}
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
                    testID="text-safety-number"
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
                    <Pressable onPress={onToggleVerified} testID="btn-verify">
                      <Chip
                        variant={
                          safety.data.verification === "verified" ? "on" : "default"
                        }
                      >
                        {setVerified.isPending
                          ? "…"
                          : safety.data.verification === "verified"
                            ? t("mobile:user.verified")
                            : t("profile.markVerified")}
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
                        {t("mobile:user.keyChanged")}
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
                {t("mobile:user.compareHint")}
              </Text>
            </View>

            <SectionTitle>{upper(t("mobile:user.safetyActionsHeading"))}</SectionTitle>
            <View style={{ paddingHorizontal: 18 }}>
              <Button
                full
                testID={isBlocked ? "btn-unblock" : "btn-block"}
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
                  ? upper(t("mobile:common.working"))
                  : isBlocked
                    ? upper(t("mobile:user.unblockUser"))
                    : upper(t("mobile:user.blockUser"))}
              </Button>
            </View>
          </View>
        )}
      </Body>
      <Ctx cr={upper(t("mobile:user.title"))} name={display} />
      {!isSelf ? (
        <BottomAction>
          <Button
            full
            testID="btn-message"
            variant="primary"
            onPress={onMessage}
            disabled={createDM.isPending}
            iconRight={<Icon.send color="#0a0907" />}
          >
            {createDM.isPending
              ? upper(t("mobile:user.opening"))
              : upper(t("mobile:user.message"))}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}

export default observer(UserProfile);
