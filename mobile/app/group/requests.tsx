import { View } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, ListRow, Group, Chip, SectionTitle, EmptyState } from "../../components/ui";
import { UserAvatar } from "../../components/UserAvatar";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { space } from "../../theme/tokens";
import {
  useGroupJoinRequests,
  useApproveJoinRequest,
  useRejectJoinRequest,
  useUserGroupsWithChannels,
} from "../../hooks/queries";
import { activeLocale } from "../../i18n";
import { useThemeRerender } from "../../components/theme";
import { useSubmitLock } from "../../hooks/useSubmitLock";

export default function JoinRequests() {
  useThemeRerender();
  const { t } = useTranslation("channels");
  const { groupId } = useRouteParams<{ groupId?: string }>();
  const router = useNav();
  const id = groupId ?? null;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const { data: requests = [], isLoading } = useGroupJoinRequests(id);
  const approve = useApproveJoinRequest(id);
  const reject = useRejectJoinRequest(id);
  // One decision at a time (#1937): claimed on the tap itself, held until it lands or fails.
  const submit = useSubmitLock();
  const decide = (mutation: typeof approve | typeof reject, requestId: string) => {
    if (!submit.begin()) {
      return;
    }
    mutation.reset();
    mutation.mutate(requestId, { onSettled: submit.end });
  };

  return (
    <Screen testID="screen-group-requests" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack} title={t("mobile:group.panel.joinRequests")} subtitle={group?.name} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl }}>
        <SectionTitle style={{ paddingHorizontal: 0 }}>
          {t("mobile:group.requests.pendingSection")}
        </SectionTitle>
        {isLoading ? <Hint>{t("common:states.loading")}</Hint> : null}
        {!isLoading && requests.length === 0 ? (
          <EmptyState testID="group-requests-empty">{t("joinRequests.empty")}</EmptyState>
        ) : null}
        {requests.length > 0 ? (
          <Group>
            {requests.map((r) => {
              const handle = `@${r.requester_username ?? r.requester_id.slice(0, 8)}`;
              return (
                <View key={r.id}>
                  <ListRow
                    testID={`row-request-${r.id}`}
                    minHeight={64}
                    glyph={<UserAvatar userId={r.requester_id} label={r.requester_username ?? r.requester_id} on="raised" />}
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
                      loading={approve.isPending && approve.variables === r.id}
                      disabled={submit.busy && !(approve.isPending && approve.variables === r.id)}
                      onPress={() => decide(approve, r.id)}
                    >
                      {t("mobile:group.requests.approve")}
                    </Chip>
                    <Chip
                      variant="danger"
                      testID={`btn-reject-${r.id}`}
                      accessibilityLabel={`${t("mobile:group.requests.declineLabel")}, ${handle}`}
                      loading={reject.isPending && reject.variables === r.id}
                      disabled={submit.busy && !(reject.isPending && reject.variables === r.id)}
                      onPress={() => decide(reject, r.id)}
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
