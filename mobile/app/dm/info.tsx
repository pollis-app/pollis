import { useState } from "react";
import { View } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Button,
  Group,
  Txt,
} from "../../components/ui";
import { ExportArchive } from "../../components/ExportArchive";
import { Icon } from "../../components/icons";
import { semantic, space, layout } from "../../theme/tokens";
import { useDMChannel, useLeaveDM } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function DMInfo() {
  const router = useNav();
  const { t } = useTranslation("mobile");
  const { id } = useRouteParams<{ id?: string }>();
  const channelId = id ?? null;
  const currentUser = appStore.currentUser;
  const [confirmLeave, setConfirmLeave] = useState(false);

  // Same `get_dm_channel` source the DM list rows come from (#907) — the
  // shared hook keys it under the ["dm"] prefix so realtime roster/DM events
  // invalidate this roster along with the list.
  const channel = useDMChannel(channelId);

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
      onSuccess: () => router.exitToTab("direct"),
    });
  };

  const members = channel.data?.members ?? [];

  return (
    <Screen testID="screen-dm-info" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack}
        title={t("conversationInfo.info")}
        backTo={t("conversationInfo.fallbackTitle")}
      />
      <Body>
        <View style={{ paddingHorizontal: space.xxl }}>
          <SectionTitle style={{ paddingHorizontal: 0 }}>
            {members.length > 0
              ? t("dm.participantsCount", { count: members.length })
              : t("dm.participants")}
          </SectionTitle>
          {channel.isLoading ? (
            <Txt variant="secondary">
              {t("common:states.loading")}
            </Txt>
          ) : null}
          {members.length > 0 ? (
            <Group>
              {members.map((m) => {
                const isMe = m.user_id === currentUser?.id;
                const handle = m.username ?? m.user_id.slice(0, 8);
                return (
                  <ListRow
                    key={m.user_id}
                    testID={`row-member-${m.user_id}`}
                    minHeight={52}
                    glyph={
                      <Avatar
                        label={handle}
                        size={layout.touchMin - 8}
                        variant={isMe ? "self" : "default"}
                      />
                    }
                    name={
                      isMe
                        ? t("conversationInfo.memberSelf", { handle })
                        : `@${handle}`
                    }
                    onPress={
                      isMe
                        ? undefined
                        : () =>
                            router.push({
                              pathname: "/user/[id]",
                              params: { id: m.user_id },
                            })
                    }
                    chevron={!isMe}
                  />
                );
              })}
            </Group>
          ) : null}
        </View>

        {/* ExportArchive pads itself (shared with conversation info). */}
        <ExportArchive conversationId={channelId ?? null} />

        <SectionTitle>{t("dm.danger")}</SectionTitle>
        <View style={{ gap: space.sm, paddingHorizontal: space.xxl }}>
          <Button
            full
            testID="btn-leave"
            variant="secondary"
            icon={<Icon.logOut size={20} color={semantic.text} />}
            onPress={onLeave}
            disabled={leave.isPending || !channelId}
          >
            {leave.isPending
              ? t("dms:settings.submitting")
              : confirmLeave
                ? t("dm.tapAgainToConfirm")
                : t("dms:settings.leave")}
          </Button>
          {leave.isError ? (
            <Txt
              variant="secondary"
              style={{ color: semantic.accent }}
            >
              {(leave.error as Error).message || t("dms:settings.leaveFailed")}
            </Txt>
          ) : null}
        </View>
      </Body>
    </Screen>
  );
}

export default observer(DMInfo);
