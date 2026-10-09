// #847 (mobile) — the one and only view of a freshly minted invite link.
//
// The server stores only `sha256(secret)`, so unlike Slack or Discord — which
// keep the code in plaintext and let you re-open it forever — this link
// genuinely cannot be shown again by anyone, including us. The card says so
// out loud instead of hiding it behind a support question later.
//
// Copy is VERIFIED (#897/#958 semantics): `Clipboard.setStringAsync` resolves
// to a boolean, and a rejected or false write surfaces as a visible failed
// state — a link that is only ever shown once and was never actually copied is
// the worst possible thing to report as a success. We deliberately do NOT
// read the clipboard back to double-check: on iOS 14+ a clipboard read fires
// the system "pasted from Pollis" banner on every tap.

import { useEffect, useRef, useState } from "react";
import { View, Text, Share } from "react-native";
import { useTranslation } from "react-i18next";
import * as Clipboard from "expo-clipboard";
import { Card, Button } from "./ui";
import { Icon } from "./icons";
import { semantic, type as ty, fonts, r, space } from "../theme/tokens";
import { activeLocale } from "../i18n";
import type { CreatedInviteLink } from "../hooks/queries";

type CopyState = "idle" | "copied" | "failed";

// How long an outcome stays on the button before it returns to idle.
const COPY_FEEDBACK_MS = 2000;

export function CreatedInviteLinkCard({ link }: { link: CreatedInviteLink }) {
  const { t } = useTranslation("channels");
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clear the outcome a couple of seconds after it appears, and never leave a
  // timer behind that would set state on an unmounted card.
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
    // Back to idle first so a second tap on an already-failed button is a
    // visible state change, and so the feedback timer restarts.
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(link.url).catch(() => false);
    setCopyState(copied ? "copied" : "failed");
  };

  const onShare = () => {
    // Native share sheet. Failures (user dismissed) are not an error state.
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
