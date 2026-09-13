import { useEffect, useRef, useState } from "react";
import { View, Text, ActivityIndicator } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Screen, Crumb, Body, Button, BottomAction, Ctx } from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import { upper } from "../../i18n";
import { invoke } from "../../lib/native";
import { restoreSession } from "../../hooks/queries/useAuth";
import { appStore } from "../../stores/appStore";
import { groupQueryKeys, type RedeemedInvite } from "../../hooks/queries";

interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

type Phase = "working" | "confirm" | "joining" | "signedOut" | "locked" | "failed";

// #847 (mobile) — where a shared invite link lands.
//
// expo-router maps `pollis://invite/<token>` here on warm AND cold starts (it
// wires `Linking.getInitialURL()` into the navigation container itself, so a
// link tapped while the app is killed still arrives as this route's initial
// screen). The desktop-rendered `https://pollis.com/invite/<token>` form needs
// iOS universal links / Android app links (associated-domains + server files)
// before it opens the app — deliberately not configured here.
//
// #1094: arriving here does NOT join anything. `pollis://invite/<token>` is
// reachable by anyone who can put a link in front of the user — an SMS, another
// app, a web page with an `<a href="pollis://…">` — and joining a group
// discloses their username and device identity to every member of it. So this
// screen resolves the session, then STOPS and asks. One tap must never be
// enough. (Slack and Discord both confirm before joining.)
//
// Deliberately no group-name preview: resolving a token to group metadata
// without redeeming would be a new oracle, and it is not what closes the hole —
// the deliberate press is.
//
// On failure this renders ONE opaque message for every cause. The Delivery
// Service deliberately cannot tell us whether a token was wrong, expired,
// revoked or used up — distinguishing them would confirm to an attacker that
// a token was real but stale.
export default function InviteLanding() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const queryClient = useQueryClient();
  const { token } = useLocalSearchParams<{ token?: string }>();
  const [phase, setPhase] = useState<Phase>("working");
  const [readyUserId, setReadyUserId] = useState<string | null>(null);

  // Cold-start arrival bypasses the boot router in app/index.tsx, so this
  // screen restores the session itself before redeeming. Runs once.
  const ran = useRef(false);
  useEffect(() => {
    if (ran.current) {
      return;
    }
    ran.current = true;
    (async () => {
      const trimmed = (token ?? "").trim();
      if (!trimmed) {
        setPhase("failed");
        return;
      }
      try {
        let userId = appStore.currentUser?.id ?? null;
        if (!userId) {
          const profile = await restoreSession();
          if (!profile) {
            setPhase("signedOut");
            return;
          }
          const snap = await invoke<UnlockStateSnapshot>("get_unlock_state");
          if (!snap.is_unlocked) {
            setPhase("locked");
            return;
          }
          userId = profile.id;
        }
        setReadyUserId(userId);
        setPhase("confirm");
      } catch (e) {
        console.warn("[invite] could not resolve the session:", e);
        setPhase("failed");
      }
    })();
  }, [token]);

  // The deliberate press. Only this redeems.
  const join = async () => {
    const trimmed = (token ?? "").trim();
    if (!trimmed || !readyUserId) {
      setPhase("failed");
      return;
    }
    setPhase("joining");
    try {
      const result = await invoke<RedeemedInvite>(
        "redeem_group_invite_link",
        { token: trimmed, userId: readyUserId },
      );
      // The redeemer just crossed into a group they could not see before —
      // invalidate broadly, like desktop's useRedeemGroupInviteLink.
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.all });
      router.replace({
        pathname: "/group/[id]",
        params: { id: result.group_id },
      });
    } catch (e) {
      console.warn("[invite] redeem failed:", e);
      setPhase("failed");
    }
  };

  const message =
    phase === "working"
      ? t("mobile:invite.checking")
      : phase === "confirm"
        ? t("mobile:invite.confirm")
        : phase === "joining"
          ? t("mobile:invite.joining")
      : phase === "signedOut"
        ? t("mobile:invite.signedOut")
        : phase === "locked"
          ? t("mobile:invite.locked")
          : t("mobile:invite.failed");

  return (
    <Screen testID="screen-invite-landing" centered>
      <Crumb
        segs={[
          { label: "POLLIS" },
          { label: t("mobile:invite.title"), leaf: true },
        ]}
      />
      <Body>
        <View
          style={{
            paddingHorizontal: 18,
            paddingTop: 24,
            gap: 12,
            alignItems: "center",
          }}
        >
          {phase === "working" || phase === "joining" ? (
            <ActivityIndicator color={semantic.accent} />
          ) : null}
          {phase === "confirm" ? (
            <Text
              testID="invite-confirm-title"
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 15,
                color: semantic.ink,
                textAlign: "center",
              }}
            >
              {t("mobile:invite.confirmTitle")}
            </Text>
          ) : null}
          <Text
            testID="invite-landing-message"
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: phase === "failed" ? semantic.danger : semantic.ink2,
              lineHeight: 19,
              textAlign: "center",
            }}
          >
            {message}
          </Text>
        </View>
      </Body>
      <Ctx cr="POLLIS" name={t("mobile:invite.groupInvite")} hideBack />
      {phase === "confirm" ? (
        <BottomAction>
          <Button
            full
            testID="btn-invite-join"
            variant="primary"
            onPress={join}
          >
            {upper(t("mobile:invite.join"))}
          </Button>
        </BottomAction>
      ) : phase === "working" || phase === "joining" ? null : (
        <BottomAction>
          <Button
            full
            testID="btn-invite-continue"
            variant="primary"
            onPress={() => router.replace("/")}
          >
            {phase === "failed"
              ? upper(t("mobile:invite.backToPollis"))
              : upper(t("mobile:invite.openPollis"))}
          </Button>
        </BottomAction>
      )}
    </Screen>
  );
}
