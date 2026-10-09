import { Text, View } from "react-native";
import { useNav } from "../pane/paneContext";
import { useTranslation } from "react-i18next";
import { Badge, Chip, Group, ListRow } from "../ui";
import { Icon } from "../icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import {
  useAcceptGroupInvite,
  useDeclineGroupInvite,
  type GroupWithChannels,
  type PendingInvite,
} from "../../hooks/queries";

// Things waiting on you across every group, shown above the Groups tab's
// channel list: join requests in groups you administer (#1216) and invites
// you have not answered yet. Renders nothing when there are none.
export function PendingRows({
  groups,
  pendingByGroup,
  invites,
}: {
  groups: GroupWithChannels[];
  pendingByGroup: Map<string, number>;
  invites: PendingInvite[];
}) {
  const { t } = useTranslation("mobile");
  // iPad two-pane: group pages open in the detail pane (useNav).
  const router = useNav();
  const acceptInvite = useAcceptGroupInvite();
  const declineInvite = useDeclineGroupInvite();
  const requestGroups = groups.filter((g) => pendingByGroup.has(g.id));

  if (requestGroups.length === 0 && invites.length === 0) {
    return null;
  }

  return (
    <View style={{ gap: space.lg, paddingBottom: space.xxl }}>
      {requestGroups.length > 0 ? (
        <View style={{ gap: space.sm }}>
          <Text accessibilityRole="header" style={[ty.section, { paddingStart: space.sm }]}>
            {t("groups.joinRequestsSection")}
          </Text>
          <Group>
            {requestGroups.map((g) => {
              const count = pendingByGroup.get(g.id) ?? 0;
              const sub = t("channels:groups.joinRequestsPending", { count });
              return (
                <ListRow
                  key={g.id}
                  testID={`row-join-requests-${g.id}`}
                  glyph={<Icon.inbox size={20} color={semantic.dim} />}
                  name={g.name}
                  sub={sub}
                  accessibilityLabel={`${g.name}, ${sub}`}
                  end={<Badge testID={`badge-join-requests-${g.id}`}>{count}</Badge>}
                  chevron
                  onPress={() =>
                    router.push({ pathname: "/group/requests", params: { groupId: g.id } })
                  }
                />
              );
            })}
          </Group>
        </View>
      ) : null}
      {invites.length > 0 ? (
        <View style={{ gap: space.sm }}>
          <Text accessibilityRole="header" style={[ty.section, { paddingStart: space.sm }]}>
            {t("groups.pendingInvites")}
          </Text>
          <Group>
            {invites.map((inv) => (
              <ListRow
                key={inv.id}
                testID={`row-invite-${inv.id}`}
                glyph={<Icon.mail size={20} color={semantic.dim} />}
                name={inv.group_name}
                sub={t("channels:invites.invitedBy", {
                  name: inv.inviter_username
                    ? `@${inv.inviter_username}`
                    : t("nav:statusBar.someone"),
                })}
                end={
                  <View style={{ flexDirection: "row", gap: space.xs }}>
                    <Chip
                      testID={`btn-decline-invite-${inv.id}`}
                      accessibilityLabel={`${t("groups.declineInviteLabel")}, ${inv.group_name}`}
                      variant="outline"
                      onPress={() => declineInvite.mutate(inv.id)}
                    >
                      {t("groups.decline")}
                    </Chip>
                    <Chip
                      testID={`btn-accept-invite-${inv.id}`}
                      accessibilityLabel={`${t("groups.acceptInviteLabel")}, ${inv.group_name}`}
                      selected
                      onPress={() => acceptInvite.mutate(inv.id)}
                    >
                      {acceptInvite.isPending ? "…" : t("groups.accept")}
                    </Chip>
                  </View>
                }
              />
            ))}
          </Group>
        </View>
      ) : null}
    </View>
  );
}
