import { useState } from "react";
import { View, Text } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, Chip, Group } from "../../components/ui";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  useGroupInviteLinks,
  useRevokeGroupInviteLink,
  type InviteLinkSummary,
} from "../../hooks/queries";
import { activeLocale } from "../../i18n";

// #847 (mobile) — review and revoke a group's shareable invite links.
//
// There is deliberately NO copy button anywhere on this screen:
// `InviteLinkSummary` carries no token because the server stores only
// `sha256(secret)` and has no token to give back. A link is copyable exactly
// once, at creation, on the invite screen.
export default function GroupInviteLinks() {
  const { t } = useTranslation("channels");
  const { groupId } = useRouteParams<{ groupId?: string }>();
  const router = useNav();
  const id = groupId ?? null;

  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);

  const { data: links = [], isLoading } = useGroupInviteLinks(id);
  const revokeLink = useRevokeGroupInviteLink(id);

  // Two-tap confirm, same pattern as channel delete in group/settings.
  const [confirmRevoke, setConfirmRevoke] = useState<string | null>(null);

  const onRevoke = (linkId: string) => {
    if (confirmRevoke !== linkId) {
      setConfirmRevoke(linkId);
      return;
    }
    revokeLink.mutate(linkId, {
      onSettled: () => setConfirmRevoke(null),
    });
  };

  const statusOf = (link: InviteLinkSummary): string => {
    // `is_live` is computed server-side so this badge cannot disagree with
    // what redemption will actually do.
    if (link.revoked_at) {
      return t("inviteLinks.statusRevoked");
    }
    if (link.is_live) {
      return t("inviteLinks.statusActive");
    }
    return t("inviteLinks.statusExpired");
  };

  const detailOf = (link: InviteLinkSummary): string => {
    const uses =
      link.max_uses != null
        ? t("inviteLinks.rowUsesOfMax", { used: link.uses, count: link.max_uses })
        : t("inviteLinks.rowUses", { count: link.uses });
    const expiry = link.expires_at
      ? t("inviteLinks.expiresOn", {
          date: new Date(link.expires_at).toLocaleDateString(activeLocale()),
        })
      : t("inviteLinks.noExpiry");
    const creator = link.creator_username
      ? t("inviteLinks.createdBy", { name: link.creator_username })
      : null;
    return [uses, expiry, creator].filter(Boolean).join(" · ");
  };

  return (
    <Screen testID="screen-group-invite-links" aboveTabBar={router.inPane} centered>
      <Header onBack={router.onBack} title={t("mobile:group.inviteLinks.title")} subtitle={group?.name} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.lg }}>
        <Hint>{t("mobile:group.inviteLinks.blurb")}</Hint>

        {links.length > 0 ? (
          <Group>
            {links.map((link) => {
              const armed = confirmRevoke === link.id;
              const status = statusOf(link);
              const detail = detailOf(link);
              return (
                <View
                  key={link.id}
                  testID={`row-invite-link-${link.id}`}
                  style={{
                    flexDirection: "row",
                    alignItems: "center",
                    gap: space.lg,
                    minHeight: 64,
                    paddingVertical: space.md,
                    paddingStart: space.xxl,
                    paddingEnd: space.xl,
                  }}
                >
                  <View
                    accessible
                    accessibilityLabel={`${status}, ${detail}`}
                    style={{ flex: 1, minWidth: 0, gap: 2 }}
                  >
                    <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
                      <Icon.link size={16} color={link.is_live ? semantic.accent : semantic.muted} />
                      <Text
                        style={{
                          fontFamily: link.is_live ? fonts.semibold : fonts.medium,
                          fontSize: 16,
                          color: link.is_live ? semantic.text : semantic.dim,
                        }}
                      >
                        {status}
                      </Text>
                    </View>
                    <Text numberOfLines={2} style={ty.secondary}>
                      {detail}
                    </Text>
                  </View>
                  {link.is_live ? (
                    <Chip
                      variant="outline"
                      selected={armed}
                      testID={`btn-revoke-invite-link-${link.id}`}
                      accessibilityLabel={
                        armed
                          ? t("mobile:group.settings.tapAgainToConfirm")
                          : t("mobile:group.inviteLinks.revokeLabel")
                      }
                      onPress={() => onRevoke(link.id)}
                    >
                      {revokeLink.isPending && armed
                        ? "…"
                        : armed
                          ? t("mobile:group.common.confirm")
                          : t("inviteLinks.revoke")}
                    </Chip>
                  ) : null}
                </View>
              );
            })}
          </Group>
        ) : null}

        {!isLoading && links.length === 0 ? <Hint>{t("mobile:group.inviteLinks.empty")}</Hint> : null}

        {revokeLink.isError ? (
          <ErrorText>
            {(revokeLink.error as Error).message ||
              t("mobile:group.inviteLinks.revokeFailed")}
          </ErrorText>
        ) : null}
      </Body>
    </Screen>
  );
}
