// The one and only view of a freshly minted invite link (#847): the server
// stores only `sha256(secret)`, so it cannot be shown again.
//
// Copy is verified: a rejected or false `setStringAsync` shows a failed state.
// The clipboard is deliberately not read back: on iOS that fires the system
// "pasted from Pollis" banner on every tap.

import { useEffect, useRef, useState } from "react";
import { View, Text, Share } from "react-native";
import { useTranslation } from "react-i18next";
import * as Clipboard from "expo-clipboard";
import { Card, Button } from "./ui";
import { Icon } from "./icons";
import { semantic, type as ty, fonts, r, space } from "../theme/tokens";
import { activeLocale } from "../i18n";
import type { CreatedInviteLink } from "../hooks/queries";
import { haptic } from "../lib/haptics";

type CopyState = "idle" | "copied" | "failed";

const COPY_FEEDBACK_MS = 2000;

export function CreatedInviteLinkCard({ link }: { link: CreatedInviteLink }) {
  const { t } = useTranslation("channels");
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (copyState === "idle") {
      return;
    }
    timer.current = setTimeout(() => setCopyState("idle"), COPY_FEEDBACK_MS);
    return () => {
      if (timer.current) {
        clearTimeout(timer.current);
      }
    };
  }, [copyState]);

  const onCopy = async () => {
    // Back to idle first so a second tap on a failed button is a visible
    // change and the feedback timer restarts.
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(link.url).catch(() => false);
    haptic(copied ? "success" : "error");
    setCopyState(copied ? "copied" : "failed");
  };

  const onShare = () => {
    // A dismissed share sheet is not an error state.
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
          style={{ flex: 1, fontFamily: fonts.semibold, fontSize: 16, color: semantic.text }}
        >
          {t("mobile:group.invite.linkCreated")}
        </Text>
      </View>
      <Text style={[ty.secondary, { lineHeight: 20 }]}>{t("mobile:group.invite.linkOnce")}</Text>
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
          style={{
            fontFamily: ty.mono.fontFamily,
            fontSize: 13,
            color: semantic.text,
          }}
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
