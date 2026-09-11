import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
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
import { useDMChannel, useLeaveDM } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { upper } from "../../i18n";

function DMInfo() {
  const router = useRouter();
  const { t } = useTranslation("mobile");
  const { id } = useLocalSearchParams<{ id?: string }>();
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
      onSuccess: () => router.replace("/(tabs)/direct"),
    });
  };

  const members = channel.data?.members ?? [];
  const directLabel = upper(t("tabs.direct"));

  return (
    <Screen testID="screen-dm-info">
      <Crumb
        segs={[
          { label: directLabel },
          { label: t("conversationInfo.info"), leaf: true },
        ]}
        end={members.length > 0 ? String(members.length) : undefined}
      />
      <Body>
        <SectionTitle>
          {upper(
            members.length > 0
              ? t("dm.participantsCount", { count: members.length })
              : t("dm.participants"),
          )}
        </SectionTitle>
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
            {t("common:states.loading")}
          </Text>
        ) : null}
        {members.map((m) => {
          const isMe = m.user_id === currentUser?.id;
          const handle = m.username ?? m.user_id.slice(0, 8);
          return (
            <ListRow
              key={m.user_id}
              testID={`row-member-${m.user_id}`}
              minHeight={54}
              glyph={<Avatar label={handle.slice(0, 2)} />}
              name={
                isMe
                  ? t("conversationInfo.memberSelf", { handle })
                  : `@${handle}`
              }
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

        <SectionTitle>{upper(t("dm.danger"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18 }}>
          <Button
            full
            testID="btn-leave"
            variant="danger"
            icon={<Icon.exit color={semantic.danger} />}
            onPress={onLeave}
            disabled={leave.isPending || !channelId}
          >
            {upper(
              leave.isPending
                ? t("dms:settings.submitting")
                : confirmLeave
                  ? t("dm.tapAgainToConfirm")
                  : t("dms:settings.leave"),
            )}
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
              {(leave.error as Error).message || t("dms:settings.leaveFailed")}
            </Text>
          ) : null}
        </View>
      </Body>
      <Ctx cr={directLabel} name={t("conversationInfo.info")} />
      <BottomAction>
        <Button
          full
          testID="btn-back-to-conversation"
          variant="subtle"
          onPress={() => router.back()}
          icon={<Icon.back color={semantic.ink} />}
        >
          {t("conversationInfo.backToConversation")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(DMInfo);
