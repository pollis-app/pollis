import { useState } from "react";
import { View, Text } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Crumb, Body, Chip, Ctx } from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  useGroupInviteLinks,
  useRevokeGroupInviteLink,
  type InviteLinkSummary,
} from "../../hooks/queries";
import { activeLocale, upper } from "../../i18n";

// #847 (mobile) — review and revoke a group's shareable invite links.
//
// There is deliberately NO copy button anywhere on this screen:
// `InviteLinkSummary` carries no token because the server stores only
// `sha256(secret)` and has no token to give back. A link is copyable exactly
// once, at creation, on the invite screen.
export default function GroupInviteLinks() {
  const { t } = useTranslation("channels");
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
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
      return upper(t("inviteLinks.statusRevoked"));
    }
    if (link.is_live) {
      return upper(t("inviteLinks.statusActive"));
    }
    return upper(t("inviteLinks.statusExpired"));
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
    <Screen testID="screen-group-invite-links" centered>
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: group?.name ?? t("mobile:group.common.fallbackName") },
          { label: t("mobile:group.inviteLinks.title"), leaf: true },
        ]}
      />
      <Body>
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 11,
            color: semantic.mute,
            lineHeight: 16,
            paddingHorizontal: 18,
            paddingTop: 6,
          }}
        >
          {t("mobile:group.inviteLinks.blurb")}
        </Text>

        {links.map((link) => {
          const armed = confirmRevoke === link.id;
          const status = statusOf(link);
          return (
            <View
              key={link.id}
              testID={`row-invite-link-${link.id}`}
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: 12,
                minHeight: 56,
                paddingVertical: 12,
                paddingHorizontal: 18,
                borderBottomWidth: 1,
                borderBottomColor: semantic.hairSoft,
              }}
            >
              <View style={{ flex: 1, minWidth: 0 }}>
                <Text
                  style={{
                    fontFamily: ty.rowN.fontFamily,
                    fontSize: 14,
                    color: link.is_live ? semantic.accent : semantic.mute,
                  }}
                >
                  [{status}]
                </Text>
                <Text
                  numberOfLines={1}
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.mute,
                    marginTop: 2,
                  }}
                >
                  {detailOf(link)}
                </Text>
              </View>
              {link.is_live ? (
                <Chip
                  variant={armed ? "on" : "default"}
                  testID={`btn-revoke-invite-link-${link.id}`}
                  accessibilityLabel={t("mobile:group.inviteLinks.revokeLabel")}
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

        {!isLoading && links.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 14,
            }}
          >
            {t("mobile:group.inviteLinks.empty")}
          </Text>
        ) : null}

        {revokeLink.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 8,
            }}
          >
            {(revokeLink.error as Error).message ||
              t("mobile:group.inviteLinks.revokeFailed")}
          </Text>
        ) : null}
      </Body>
      <Ctx
        cr={group?.name ?? upper(t("mobile:group.common.fallbackName"))}
        name={t("mobile:group.inviteLinks.title")}
      />
    </Screen>
  );
}
