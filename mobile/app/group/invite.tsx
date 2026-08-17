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
  Chip,
  SectionTitle,
  ListRow,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { CreatedInviteLinkCard } from "../../components/CreatedInviteLinkCard";
import {
  useSendGroupInvite,
  useUserGroupsWithChannels,
  useCreateGroupInviteLink,
  type CreatedInviteLink,
} from "../../hooks/queries";

// Expiry presets, mirroring desktop's InviteLinkManager (#847). A fixed list
// rather than a date picker on purpose — every preset here is one a person
// actually asks for.
const EXPIRY_OPTIONS: { id: string; label: string; hours: number | null }[] = [
  { id: "24h", label: "24 HOURS", hours: 24 },
  { id: "7d", label: "7 DAYS", hours: 24 * 7 },
  { id: "30d", label: "30 DAYS", hours: 24 * 30 },
  { id: "never", label: "NEVER", hours: null },
];

const USES_OPTIONS: { id: string; label: string; uses: number | null }[] = [
  { id: "1", label: "1 USE", uses: 1 },
  { id: "10", label: "10 USES", uses: 10 },
  { id: "unlimited", label: "UNLIMITED", uses: null },
];

export default function InviteToGroup() {
  const router = useRouter();
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const [identifier, setIdentifier] = useState("");
  const sendInvite = useSendGroupInvite(groupId ?? null);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);

  // ── #847 shareable link mint state ──────────────────────────────────
  const [expiryHours, setExpiryHours] = useState<number | null>(24 * 7);
  const [maxUses, setMaxUses] = useState<number | null>(null);
  const [created, setCreated] = useState<CreatedInviteLink | null>(null);
  const createLink = useCreateGroupInviteLink(groupId ?? null);

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

  const onCreateLink = () => {
    createLink.mutate(
      { expiresInHours: expiryHours, maxUses },
      {
        onSuccess: (link) => setCreated(link),
      },
    );
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

        <SectionTitle>SHAREABLE LINK</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            Anyone with the link can join directly — no approval step. The
            link is shown exactly once, right after you create it.
          </Text>

          <Text style={[ty.label, { paddingTop: 6 }]}>EXPIRES AFTER</Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
            {EXPIRY_OPTIONS.map((opt) => (
              <Chip
                key={opt.id}
                variant={expiryHours === opt.hours ? "on" : "default"}
                testID={`chip-expiry-${opt.id}`}
                onPress={() => setExpiryHours(opt.hours)}
              >
                {opt.label}
              </Chip>
            ))}
          </View>

          <Text style={[ty.label, { paddingTop: 6 }]}>MAXIMUM USES</Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
            {USES_OPTIONS.map((opt) => (
              <Chip
                key={opt.id}
                variant={maxUses === opt.uses ? "on" : "default"}
                testID={`chip-uses-${opt.id}`}
                onPress={() => setMaxUses(opt.uses)}
              >
                {opt.label}
              </Chip>
            ))}
          </View>

          <View style={{ paddingTop: 8 }}>
            <Button
              full
              testID="btn-create-invite-link"
              onPress={onCreateLink}
              disabled={createLink.isPending}
              icon={<Icon.link color={semantic.ink} />}
            >
              {createLink.isPending ? "CREATING…" : "CREATE INVITE LINK"}
            </Button>
          </View>

          {createLink.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
              }}
            >
              {(createLink.error as Error).message ||
                "Couldn't create invite link."}
            </Text>
          ) : null}

          {created ? <CreatedInviteLinkCard link={created} /> : null}
        </View>

        <ListRow
          testID="row-manage-invite-links"
          minHeight={48}
          glyph={<Icon.link color={semantic.mute} />}
          name="Manage invite links"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub="Review and revoke existing links"
          onPress={() =>
            groupId &&
            router.push({
              pathname: "/group/invite-links",
              params: { groupId },
            })
          }
          end={<Icon.fwd color={semantic.mute} />}
        />
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
