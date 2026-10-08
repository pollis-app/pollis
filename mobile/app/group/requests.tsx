import { View } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, ListRow, Group, Avatar, Chip, SectionTitle } from "../../components/ui";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { space } from "../../theme/tokens";
import {
  useGroupJoinRequests,
  useApproveJoinRequest,
  useRejectJoinRequest,
  useUserGroupsWithChannels,
} from "../../hooks/queries";
import { activeLocale } from "../../i18n";

export default function JoinRequests() {
  const { t } = useTranslation("channels");
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const { data: requests = [], isLoading } = useGroupJoinRequests(id);
  const approve = useApproveJoinRequest(id);
  const reject = useRejectJoinRequest(id);

  return (
    <Screen testID="screen-group-requests">
      <Header title={t("mobile:group.panel.joinRequests")} subtitle={group?.name} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl }}>
        <SectionTitle style={{ paddingHorizontal: 0 }}>
          {t("mobile:group.requests.pendingSection")}
        </SectionTitle>
        {isLoading ? <Hint>{t("common:states.loading")}</Hint> : null}
        {!isLoading && requests.length === 0 ? <Hint>{t("joinRequests.empty")}</Hint> : null}
        {requests.length > 0 ? (
          <Group>
            {requests.map((r) => {
              const handle = `@${r.requester_username ?? r.requester_id.slice(0, 8)}`;
              return (
                <View key={r.id}>
                  <ListRow
                    testID={`row-request-${r.id}`}
                    minHeight={64}
                    glyph={<Avatar label={r.requester_username ?? r.requester_id} />}
                    name={handle}
                    sub={t("mobile:group.requests.requested", {
                      date: new Date(r.created_at).toLocaleDateString(activeLocale()),
                    })}
                  />
                  <View
                    style={{
                      flexDirection: "row",
                      gap: space.sm,
                      paddingStart: 70,
                      paddingEnd: space.xl,
                      paddingBottom: space.sm,
                    }}
                  >
                    <Chip
                      selected
                      testID={`btn-approve-${r.id}`}
                      accessibilityLabel={`${t("mobile:group.requests.approveLabel")}, ${handle}`}
                      onPress={() => approve.mutate(r.id)}
                    >
                      {approve.isPending ? "…" : t("mobile:group.requests.approve")}
                    </Chip>
                    <Chip
                      variant="outline"
                      testID={`btn-reject-${r.id}`}
                      accessibilityLabel={`${t("mobile:group.requests.declineLabel")}, ${handle}`}
                      onPress={() => reject.mutate(r.id)}
                    >
                      {t("mobile:group.requests.decline")}
                    </Chip>
                  </View>
                </View>
              );
            })}
          </Group>
        ) : null}
        {approve.isError || reject.isError ? (
          <View style={{ paddingTop: space.md }}>
            <ErrorText>
              {((approve.error ?? reject.error) as Error).message ||
                t("mobile:group.requests.processFailed")}
            </ErrorText>
          </View>
        ) : null}
      </Body>
    </Screen>
  );
}
