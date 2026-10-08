import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  Avatar,
  Chip,
  Group,
  Txt,
} from "../../components/ui";
import { semantic, fonts, space, layout } from "../../theme/tokens";
import {
  useDMRequests,
  useAcceptDMRequest,
  useBlockUser,
} from "../../hooks/queries";

/**
 * Message requests, opened from the single "Message requests" row on the
 * Direct tab. The same rows, hooks and actions the Direct tab used to render
 * inline: Accept, or Block the sender — mirrors desktop's RequestsPage /
 * dm-request bar (there is no decline command in pollis-core).
 */
export default function DMRequests() {
  const { t } = useTranslation("mobile");
  const { data: requests = [], isLoading } = useDMRequests();
  const acceptRequest = useAcceptDMRequest();
  const blockUser = useBlockUser();

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
          <Group>
            {requests.map((d) => {
              const handle =
                d.user2_identifier || t("dms:profile.fallbackName");
              return (
                <View
                  key={d.id}
                  testID={`row-request-${d.id}`}
                  style={{
                    flexDirection: "row",
                    alignItems: "center",
                    flexWrap: "wrap",
                    gap: space.lg,
                    minHeight: 64,
                    paddingVertical: space.sm,
                    paddingStart: space.xxl,
                    paddingEnd: space.md,
                  }}
                >
                  <View
                    accessible
                    accessibilityLabel={`@${handle}, ${t("direct.wantsToMessage")}`}
                    style={{
                      flexDirection: "row",
                      alignItems: "center",
                      gap: space.lg,
                      flex: 1,
                      minWidth: 160,
                    }}
                  >
                    <Avatar label={handle} size={layout.touchMin} />
                    <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
                      <Text
                        numberOfLines={1}
                        style={{
                          fontFamily: fonts.semibold,
                          fontSize: 16,
                          color: semantic.text,
                        }}
                      >
                        @{handle}
                      </Text>
                      <Txt variant="secondary" numberOfLines={2}>
                        {t("direct.wantsToMessage")}
                      </Txt>
                    </View>
                  </View>
                  <View style={{ flexDirection: "row", gap: space.sm }}>
                    <Chip
                      testID={`btn-block-request-${d.id}`}
                      accessibilityLabel={t("direct.blockSender")}
                      variant="outline"
                      disabled={blockUser.isPending}
                      onPress={() => {
                        if (d.user2_id) {
                          blockUser.mutate(d.user2_id);
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
        ) : null}
      </Body>
    </Screen>
  );
}
