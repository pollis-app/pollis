import { View, Text } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Ctx,
} from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useGroupJoinRequests,
  useApproveJoinRequest,
  useRejectJoinRequest,
  useUserGroupsWithChannels,
} from "../../hooks/queries";
import { activeLocale, upper } from "../../i18n";

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
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: group?.name ?? t("mobile:group.common.fallbackName") },
          { label: t("nav:breadcrumb.requests"), leaf: true },
        ]}
        end={String(requests.length)}
      />
      <Body>
        <SectionTitle>{upper(t("mobile:group.requests.pendingSection"))}</SectionTitle>
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("common:states.loading")}
          </Text>
        ) : null}
        {!isLoading && requests.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("joinRequests.empty")}
          </Text>
        ) : null}
        {requests.map((r) => {
          const handle = r.requester_username ?? r.requester_id.slice(0, 8);
          return (
            <ListRow
              key={r.id}
              testID={`row-request-${r.id}`}
              minHeight={54}
              glyph={<Avatar label={handle.slice(0, 2)} />}
              name={`@${handle}`}
              nameStyle={{ fontSize: 14 }}
              sub={t("mobile:group.requests.requested", {
                date: new Date(r.created_at).toLocaleDateString(activeLocale()),
              })}
              end={
                <View style={{ flexDirection: "row", gap: 6 }}>
                  <Chip
                    testID={`btn-reject-${r.id}`}
                    accessibilityLabel={t("mobile:group.requests.declineLabel")}
                    onPress={() => reject.mutate(r.id)}
                  >
                    {t("joinRequests.reject")}
                  </Chip>
                  <Chip
                    variant="on"
                    testID={`btn-approve-${r.id}`}
                    accessibilityLabel={t("mobile:group.requests.approveLabel")}
                    onPress={() => approve.mutate(r.id)}
                  >
                    {approve.isPending ? "…" : t("joinRequests.approve")}
                  </Chip>
                </View>
              }
            />
          );
        })}
        {(approve.isError || reject.isError) ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {((approve.error ?? reject.error) as Error).message ||
              t("mobile:group.requests.processFailed")}
          </Text>
        ) : null}
      </Body>
      <Ctx
        cr={group?.name ?? upper(t("mobile:group.common.fallbackName"))}
        name={t("group.joinRequests")}
      />
    </Screen>
  );
}
