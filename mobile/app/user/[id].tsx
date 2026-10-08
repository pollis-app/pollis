import { View, Text } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { Trans, useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  Avatar,
  Card,
  Chip,
  Button,
  BottomAction,
  Group,
  ListRow,
  Txt,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, fonts, space } from "../../theme/tokens";
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
  const router = useNav();
  const { openConversation } = useOpenConversation();
  const { id } = useRouteParams<{ id: string }>();
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
    !!peerId && (blockedUsers.data ?? []).some((b) => b.user_id === peerId);
  const block = useBlockUser();
  const unblock = useUnblockUser();

  const handle =
    profile.data?.username ?? peerId ?? t("mobile:user.fallbackHandle");
  const display = profile.data?.preferred_name || handle;

  const onMessage = () => {
    if (!peerId) {
      return;
    }
    createDM.mutate(
      { memberIds: [peerId] },
      {
        onSuccess: (channel) => {
          // Phones: replace this profile with the DM. iPad: the DM opens
          // in the Direct tab's two-pane.
          openConversation({ id: channel.id, kind: "dm" }, { replace: true });
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

  const verification = safety.data?.verification;
  const blockPending = block.isPending || unblock.isPending;

  return (
    <Screen testID="screen-user" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack} title={t("profile.fallbackTitle")} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xl }}>
        <Card
          surface="panel"
          style={{ flexDirection: "row", alignItems: "center", gap: space.xl }}
        >
          <Avatar
            label={handle}
            size="lg"
            variant={isSelf ? "self" : "default"}
          />
          <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
            <Txt variant="title" accessibilityRole="header">
              {display}
            </Txt>
            <Txt variant="secondary" style={{ color: semantic.muted }}>
              @{handle}
            </Txt>
          </View>
        </Card>

        {isSelf ? (
          <Txt variant="secondary">
            <Trans
              t={t}
              i18nKey="mobile:user.selfNote"
              components={{
                link: (
                  <Text
                    accessibilityRole="link"
                    onPress={() => router.push("/self/user-settings")}
                    style={{
                      color: semantic.accent,
                      fontFamily: fonts.semibold,
                    }}
                  />
                ),
              }}
            />
          </Txt>
        ) : (
          <>
            <View style={{ gap: space.sm }}>
              <SectionTitle
                style={{
                  paddingHorizontal: 0,
                  paddingTop: space.sm,
                  paddingBottom: 0,
                }}
              >
                {t("profile.safetyNumber")}
              </SectionTitle>
              {safety.isLoading ? (
                <Txt variant="secondary">
                  {t("mobile:user.computing")}
                </Txt>
              ) : safety.isError ? (
                <Txt variant="secondary">
                  {(safety.error as Error).message ||
                    t("mobile:user.safetyNumberFailed")}
                </Txt>
              ) : safety.data ? (
                <Card
                  style={{ gap: space.lg }}
                >
                  <Text
                    selectable
                    testID="text-safety-number"
                    style={{
                      fontFamily: fonts.mono400,
                      fontSize: 14,
                      lineHeight: 22,
                      color: semantic.text,
                      letterSpacing: 0.4,
                    }}
                  >
                    {safety.data.combined}
                  </Text>
                  <View
                    style={{
                      flexDirection: "row",
                      flexWrap: "wrap",
                      alignItems: "center",
                      gap: space.sm,
                    }}
                  >
                    <Chip
                      testID="btn-verify"
                      selected={verification === "verified"}
                      variant="outline"
                      onPress={onToggleVerified}
                      disabled={setVerified.isPending}
                      accessibilityLabel={
                        verification === "verified"
                          ? t("profile.removeVerification")
                          : t("profile.markVerified")
                      }
                    >
                      {setVerified.isPending
                        ? t("mobile:common.working")
                        : verification === "verified"
                          ? t("mobile:user.verified")
                          : t("profile.markVerified")}
                    </Chip>
                    {verification === "changed" ? (
                      <View
                        style={{
                          flexDirection: "row",
                          alignItems: "center",
                          gap: space.xs,
                          flex: 1,
                          minWidth: 160,
                        }}
                      >
                        <Icon.alert size={16} color={semantic.accent} />
                        <Txt
                          variant="secondary"
                          style={{ flex: 1, color: semantic.text }}
                        >
                          {t("mobile:user.keyChanged")}
                        </Txt>
                      </View>
                    ) : null}
                  </View>
                </Card>
              ) : null}
              <Txt
                variant="meta"
                style={{ lineHeight: 17 }}
              >
                {t("mobile:user.compareHint")}
              </Txt>
            </View>

            {/* Destructive actions: their own group at the end, told apart by
                label and icon (danger is the accent — no third hue). */}
            <Group title={t("mobile:user.safetyActionsHeading")}>
              <ListRow
                testID="btn-report-user"
                glyph={<Icon.flag size={20} color={semantic.text} />}
                name={t("chat:report.titleUser", { name: `@${handle}` })}
                chevron
                onPress={() => {
                  if (peerId) {
                    router.push({
                      pathname: "/report",
                      params: { userId: peerId },
                    });
                  }
                }}
              />
              <ListRow
                testID={isBlocked ? "btn-unblock" : "btn-block"}
                glyph={<Icon.userX size={20} color={semantic.text} />}
                name={
                  blockPending
                    ? t("mobile:common.working")
                    : isBlocked
                      ? t("mobile:user.unblockUser")
                      : t("mobile:user.blockUser")
                }
                onPress={onToggleBlock}
                disabled={blockPending}
              />
            </Group>
          </>
        )}
      </Body>
      {!isSelf ? (
        <BottomAction>
          <Button
            full
            testID="btn-message"
            variant="primary"
            onPress={onMessage}
            disabled={createDM.isPending}
            icon={<Icon.messageCircle size={20} color={semantic.onAccent} />}
          >
            {createDM.isPending
              ? t("mobile:user.opening")
              : t("mobile:user.message")}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}

export default observer(UserProfile);
