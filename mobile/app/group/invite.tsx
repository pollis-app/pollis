import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useSendGroupInvite,
  useUserGroupsWithChannels,
} from "../../hooks/queries";

export default function InviteToGroup() {
  const router = useRouter();
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const [identifier, setIdentifier] = useState("");
  const sendInvite = useSendGroupInvite(groupId ?? null);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);

  const onSend = () => {
    const trimmed = identifier.trim();
    if (!trimmed) {
      return;
    }
    sendInvite.mutate(trimmed, {
      onSuccess: () => {
        setIdentifier("");
        router.back();
      },
    });
  };

  return (
    <Screen testID="screen-group-invite" centered>
      <Crumb
        segs={[
          { label: "GROUPS" },
          { label: group?.name ?? "Group" },
          { label: "Invite", leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 8 }}>
          <Text style={ty.label}>USERNAME OR EMAIL</Text>
          <Field
            amber
            value={identifier}
            onChangeText={setIdentifier}
            testID="input-user-search"
            accessibilityLabel="Username or email"
            icon={<Icon.at color={semantic.mute} />}
          />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            They'll see this invite in their Pending section the next time
            they open Pollis. Only admins of {group?.name ?? "this group"}{" "}
            can invite — if you're not one, the server will reject this.
          </Text>
          {sendInvite.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                paddingTop: 8,
              }}
            >
              {(sendInvite.error as Error).message || "Couldn't send invite."}
            </Text>
          ) : null}
          {sendInvite.isSuccess ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.accent,
                paddingTop: 8,
              }}
            >
              Invite sent.
            </Text>
          ) : null}
        </View>
      </Body>
      <Ctx cr={group?.name ?? "GROUP"} name="Invite a member" />
      <BottomAction>
        <Button
          full
          testID="btn-send-invite"
          variant="primary"
          onPress={onSend}
          disabled={!identifier.trim() || sendInvite.isPending}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {sendInvite.isPending ? "SENDING…" : "SEND INVITE"}
        </Button>
        <Button
          variant="subtle"
          full
          testID="btn-cancel"
          onPress={() => router.back()}
        >
          Cancel
        </Button>
      </BottomAction>
    </Screen>
  );
}
