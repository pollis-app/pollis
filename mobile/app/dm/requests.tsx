import { useMemo, useState } from "react";
import { View, useWindowDimensions } from "react-native";
import { Text } from "../../components/Text";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  Chip,
  Group,
  Txt,
} from "../../components/ui";
import { UserAvatar } from "../../components/UserAvatar";
import { semantic, fonts, space, layout } from "../../theme/tokens";
import {
  useDMRequests,
  useAcceptDMRequest,
  useBlockUser,
  useUserProfiles,
} from "../../hooks/queries";
import { useThemeRerender } from "../../components/theme";
import { dismissConversationNotifications } from "../../lib/push";
import { personName, stackRowActions } from "../../lib/personName";

/** Message requests: Accept or Block (pollis-core has no decline). */
export default function DMRequests() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const { data: requests = [], isLoading } = useDMRequests();
  const acceptRequest = useAcceptDMRequest();
  const blockUser = useBlockUser();
  // Display names (#1971), one batched read.
  const senderIds = useMemo(
    () => requests.flatMap((d) => (d.user2_id ? [d.user2_id] : [])),
    [requests],
  );
  const { data: profiles } = useUserProfiles(senderIds);
  // Narrow rows put Block / Accept under the name, so the name keeps the width (#1971).
  const { width: windowWidth, fontScale } = useWindowDimensions();
  const [listWidth, setListWidth] = useState(windowWidth - 2 * space.xxl);
  const stacked = stackRowActions(listWidth, fontScale);

  return (
    <Screen testID="screen-dm-requests">
      <Header title={t("direct.requests")} backTo={t("tabs.direct")} />
      <Body contentContainerStyle={{ padding: space.xxl }}>
        {!isLoading && requests.length === 0 ? (
          <Txt variant="secondary">
            {t("dms:requests.empty")}
          </Txt>
        ) : null}
        {requests.length > 0 ? (
          <View onLayout={(e) => setListWidth(e.nativeEvent.layout.width)}>
            <Group>
              {requests.map((d) => {
                const label = personName(
                  {
                    username: d.user2_identifier,
                    preferred_name: d.user2_id
                      ? profiles?.get(d.user2_id)?.preferred_name
                      : undefined,
                  },
                  t("dms:profile.fallbackName"),
                );
                return (
                  <View
                    key={d.id}
                    testID={`row-request-${d.id}`}
                    style={{
                      flexDirection: stacked ? "column" : "row",
                      alignItems: stacked ? "stretch" : "center",
                      gap: stacked ? space.sm : space.lg,
                      minHeight: 64,
                      paddingVertical: space.sm,
                      paddingStart: space.xxl,
                      paddingEnd: space.md,
                    }}
                  >
                    <View
                      accessible
                      accessibilityLabel={`${label.name}, ${t("direct.wantsToMessage")}`}
                      style={{
                        flexDirection: "row",
                        alignItems: "center",
                        gap: space.lg,
                        flex: stacked ? undefined : 1,
                        minWidth: 0,
                      }}
                    >
                      <UserAvatar
                        userId={d.user2_id}
                        avatarKey={d.user2_avatar_url}
                        label={label.name}
                        size={layout.touchMin}
                      />
                      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
                        <Text
                          testID={`text-request-name-${d.id}`}
                          numberOfLines={1}
                          ellipsizeMode={label.isHandle ? "middle" : "tail"}
                          style={{
                            fontFamily: fonts.semibold,
                            fontSize: 16,
                            color: semantic.text,
                          }}
                        >
                          {label.name}
                        </Text>
                        <Txt variant="secondary" numberOfLines={2}>
                          {t("direct.wantsToMessage")}
                        </Txt>
                      </View>
                    </View>
                    <View
                      style={{
                        flexDirection: "row",
                        flexWrap: "wrap",
                        gap: space.sm,
                        // Stacked: under the name, past the avatar.
                        paddingStart: stacked ? layout.touchMin + space.lg : 0,
                      }}
                    >
                      <Chip
                        testID={`btn-block-request-${d.id}`}
                        accessibilityLabel={t("direct.blockSender")}
                        variant="danger"
                        disabled={blockUser.isPending}
                        onPress={() => {
                          if (d.user2_id) {
                            blockUser.mutate(d.user2_id, {
                              // #1734: the request's notification leaves this tray now; the DS
                              // clears it from the other devices.
                              onSuccess: () => {
                                void dismissConversationNotifications(d.id);
                              },
                            });
                          }
                        }}
                      >
                        {blockUser.isPending
                          ? t("nav:dmRequest.blocking")
                          : t("nav:dmRequest.block")}
                      </Chip>
                      <Chip
                        testID={`btn-accept-request-${d.id}`}
                        accessibilityLabel={t("direct.acceptRequest")}
                        variant="solid"
                        disabled={acceptRequest.isPending}
                        onPress={() => acceptRequest.mutate(d.id)}
                      >
                        {acceptRequest.isPending
                          ? t("nav:dmRequest.accepting")
                          : t("nav:dmRequest.accept")}
                      </Chip>
                    </View>
                  </View>
                );
              })}
            </Group>
          </View>
        ) : null}
      </Body>
    </Screen>
  );
}
