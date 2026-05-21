import { View, Text } from "react-native";
import { useLocalSearchParams } from "expo-router";
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

export default function JoinRequests() {
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const { data: requests = [], isLoading } = useGroupJoinRequests(id);
  const approve = useApproveJoinRequest(id);
  const reject = useRejectJoinRequest(id);

  return (
    <Screen>
      <Crumb
        segs={[
          { label: "GROUPS" },
          { label: group?.name ?? "Group" },
          { label: "Requests", leaf: true },
        ]}
        end={String(requests.length)}
      />
      <Body>
        <SectionTitle>PENDING REQUESTS</SectionTitle>
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
            Loading…
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
            No pending requests.
          </Text>
        ) : null}
        {requests.map((r) => {
          const handle = r.requester_username ?? r.requester_id.slice(0, 8);
          return (
            <ListRow
              key={r.id}
              minHeight={54}
              glyph={<Avatar label={handle.slice(0, 2)} />}
              name={`@${handle}`}
              nameStyle={{ fontSize: 14 }}
              sub={`requested ${new Date(r.created_at).toLocaleDateString()}`}
              end={
                <View style={{ flexDirection: "row", gap: 6 }}>
                  <Chip onPress={() => reject.mutate(r.id)}>Decline</Chip>
                  <Chip variant="on" onPress={() => approve.mutate(r.id)}>
                    {approve.isPending ? "…" : "Approve"}
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
              "Couldn't process the request."}
          </Text>
        ) : null}
      </Body>
      <Ctx cr={group?.name ?? "GROUP"} name="Join requests" />
    </Screen>
  );
}
