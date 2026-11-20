import { View } from "react-native";
import { Text } from "../../components/Text";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { Trans, useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  Card,
  Button,
  BottomAction,
  Group,
  ListRow,
  Txt,
} from "../../components/ui";
import { UserAvatar } from "../../components/UserAvatar";
import { BotBadge } from "../../components/BotBadge";
import { AppMark } from "../../components/bots/AppMark";
import { Icon } from "../../components/icons";
import { semantic, fonts, space } from "../../theme/tokens";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import {
  useCreateDM,
  useBlockedUsers,
  useBlockUser,
  useUnblockUser,
  useGroupApps,
  useGroupMembers,
  useUserGroupsWithChannels,
} from "../../hooks/queries";
import { isBotUser } from "../../lib/bots";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useThemeRerender } from "../../components/theme";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";

interface RawProfile {
  id: string;
  username?: string;
  preferred_name?: string;
  avatar_url?: string;
}

function UserProfile() {
  useThemeRerender();
  const { t } = useTranslation("dms");
  const router = useNav();
  const { openConversation } = useOpenConversation();
  // `groupId`: opened from inside a group, which shows the person by their alias there (#2003).
  const { id, groupId } = useRouteParams<{ id: string; groupId?: string }>();
  const peerId = id ?? null;
  const alias = useGroupAliases(groupId ?? null).get(peerId ?? "");
  const currentUser = appStore.currentUser;
  const isSelf = currentUser?.id === peerId;
  // A bot seen from its group (#1182, #2005): the Bot badge, who manages it, and its App's marks.
  const { data: groupMembers = [] } = useGroupMembers(groupId ?? null);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: groupApps = [] } = useGroupApps(groupId ?? null);
  const isBot = isBotUser(groupMembers.find((m) => m.user_id === peerId));
  const app = isBot ? groupApps.find((a) => a.botUserId === peerId) : undefined;
  const groupName = groups.find((g) => g.id === groupId)?.name ?? t("mobile:group.common.fallbackName");

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

  const createDM = useCreateDM();

  const blockedUsers = useBlockedUsers();
  const isBlocked =
    !!peerId && (blockedUsers.data ?? []).some((b) => b.user_id === peerId);
  const block = useBlockUser();
  const unblock = useUnblockUser();

  const handle =
    profile.data?.username ?? peerId ?? t("mobile:user.fallbackHandle");
  const display = alias?.display_name || profile.data?.preferred_name || handle;

  const onMessage = () => {
    if (!peerId) {
      return;
    }
    createDM.mutate(
      { memberIds: [peerId] },
      {
        onSuccess: (channel) => {
          // Phones replace this profile; iPad opens the two-pane.
          openConversation({ id: channel.id, kind: "dm" }, { replace: true });
        },
      },
    );
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

  const blockPending = block.isPending || unblock.isPending;

  return (
    <Screen testID="screen-user" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack} title={t("profile.fallbackTitle")} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xl }}>
        <Card
          surface="panel"
          style={{ flexDirection: "row", alignItems: "center", gap: space.xl }}
        >
          <UserAvatar
            testID="avatar-user"
            userId={peerId}
            avatarKey={profile.data ? profile.data.avatar_url ?? null : undefined}
            aliasKey={alias?.avatar_url}
            label={handle}
            size="lg"
            variant={isSelf ? "self" : "default"}
            on="panel"
          />
          <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
            <Txt testID="text-user-name" variant="title" accessibilityRole="header">
              {display}
            </Txt>
            <Txt variant="secondary" style={{ color: semantic.muted }}>
              @{handle}
            </Txt>
            {isBot ? (
              <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.sm, paddingTop: space.xs }}>
                <BotBadge testID="badge-bot-profile" />
                {app?.development ? <AppMark kind="development" testID="mark-bot-profile-dev" /> : null}
                {app?.cannotSendOutside ? <AppMark kind="noEgress" testID="mark-bot-profile-no-egress" /> : null}
              </View>
            ) : null}
          </View>
        </Card>
        {isBot ? (
          <Txt testID="text-bot-profile-managed" variant="secondary">
            {t("mobile:bots.managedBy", { group: groupName })}
          </Txt>
        ) : null}

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
            {/* Destructive actions: their own group, by label and icon. */}
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
