import { View, Text } from "react-native";
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
import { useBlockedUsers, useUnblockUser } from "../../hooks/queries";

export default function Blocked() {
  const { data: blocked = [], isLoading } = useBlockedUsers();
  const unblock = useUnblockUser();

  return (
    <Screen testID="screen-self-blocked" centered>
      <Crumb
        segs={[{ label: "SELF" }, { label: "Blocked", leaf: true }]}
        end={String(blocked.length)}
      />
      <Body>
        <SectionTitle>BLOCKED USERS</SectionTitle>
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
        {!isLoading && blocked.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            You haven't blocked anyone.
          </Text>
        ) : null}
        {blocked.map((b) => {
          const handle = b.blocked_username ?? b.blocked_id.slice(0, 8);
          return (
            <ListRow
              key={b.blocked_id}
              testID={`row-blocked-${b.blocked_id}`}
              minHeight={54}
              glyph={<Avatar label={handle.slice(0, 2)} />}
              name={`@${handle}`}
              nameStyle={{ fontSize: 14 }}
              sub={`blocked ${new Date(b.created_at).toLocaleDateString()}`}
              end={
                <Chip
                  testID={`btn-unblock-${b.blocked_id}`}
                  accessibilityLabel="Unblock"
                  onPress={() => unblock.mutate(b.blocked_id)}
                >
                  {unblock.isPending ? "…" : "Unblock"}
                </Chip>
              }
            />
          );
        })}
        {unblock.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(unblock.error as Error).message || "Couldn't unblock."}
          </Text>
        ) : null}
      </Body>
      <Ctx cr="SELF" name="Blocked" />
    </Screen>
  );
}
