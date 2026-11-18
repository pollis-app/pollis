// #847: the only view of a fresh invite link (the server keeps only `sha256(secret)`). Copy is VERIFIED (#897), never read back.

import { View, Share } from "react-native";
import { Text } from "./Text";
import { useTranslation } from "react-i18next";
import { Card, Button } from "./ui";
import { Icon } from "./icons";
import { semantic, type as ty, r, space, scriptLineHeight } from "../theme/tokens";
import { activeLocale } from "../i18n";
import type { CreatedInviteLink } from "../hooks/queries";
import { useCopyFeedback } from "../hooks/useCopyFeedback";

export function CreatedInviteLinkCard({ link }: { link: CreatedInviteLink }) {
  const { t } = useTranslation("channels");
  const { copyState, copy } = useCopyFeedback();
  const onCopy = () => copy(link.url);

  const onShare = () => {
    Share.share({ message: link.url }).catch(() => {});
  };

  const bounds: string[] = [];
  if (link.max_uses != null) {
    bounds.push(t("inviteLinks.maxUses", { count: link.max_uses }));
  }
  if (link.expires_at) {
    bounds.push(
      t("inviteLinks.expiresOn", {
        date: new Date(link.expires_at).toLocaleString(activeLocale()),
      }),
    );
  }
  const boundsLabel =
    bounds.length > 0 ? bounds.join(" · ") : t("inviteLinks.unbounded");

  return (
    <Card style={{ gap: space.md }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
        <Icon.check size={16} color={semantic.accent} />
        <Text
          accessibilityRole="header"
          style={[ty.rowTitle, { flex: 1 }]}
        >
          {t("mobile:group.invite.linkCreated")}
        </Text>
      </View>
      <Text style={[ty.secondary, { lineHeight: scriptLineHeight(20, ty.secondary.fontSize) }]}>{t("mobile:group.invite.linkOnce")}</Text>
      <View
        style={{
          backgroundColor: semantic.panel,
          paddingVertical: space.md,
          paddingHorizontal: space.lg,
          borderRadius: r.md,
        }}
      >
        <Text
          testID="created-invite-link-url"
          selectable
          style={[ty.mono, { color: semantic.text }]}
        >
          {link.url}
        </Text>
      </View>
      <View style={{ flexDirection: "row", gap: space.sm }}>
        <View style={{ flex: 1 }}>
          <Button
            full
            testID="btn-copy-invite-link"
            surface="raised"
            variant={copyState === "failed" ? "secondary" : "primary"}
            onPress={onCopy}
            accessibilityLabel={
              copyState === "copied"
                ? t("inviteLinks.copiedLabel")
                : copyState === "failed"
                  ? t("inviteLinks.copyFailedLabel")
                  : t("inviteLinks.copyLabel")
            }
            icon={
              copyState === "copied" ? (
                <Icon.check size={18} color={semantic.onAccent} />
              ) : copyState === "failed" ? (
                <Icon.alert size={18} color={semantic.text} />
              ) : (
                <Icon.copy size={18} color={semantic.onAccent} />
              )
            }
          >
            {copyState === "copied"
              ? t("inviteLinks.copied")
              : copyState === "failed"
                ? t("inviteLinks.copyFailed")
                : t("inviteLinks.copy")}
          </Button>
        </View>
        <View style={{ flex: 1 }}>
          <Button
            full
            testID="btn-share-invite-link"
            surface="raised"
            onPress={onShare}
            icon={<Icon.share size={18} color={semantic.text} />}
          >
            {t("mobile:group.invite.share")}
          </Button>
        </View>
      </View>
      <Text style={ty.meta}>{boundsLabel}</Text>
    </Card>
  );
}
