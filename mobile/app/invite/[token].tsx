import { useEffect, useRef, useState } from "react";
import { View, Text, ActivityIndicator } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import { Screen, Crumb, Body, Button, BottomAction, Ctx } from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import { invoke } from "../../lib/native";
import { restoreSession } from "../../hooks/queries/useAuth";
import { appStore } from "../../stores/appStore";
import { groupQueryKeys, type RedeemedInvite } from "../../hooks/queries";

interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

type Phase = "working" | "signedOut" | "locked" | "failed";

// #847 (mobile) — where a shared invite link lands.
//
// expo-router maps `pollis://invite/<token>` here on warm AND cold starts (it
// wires `Linking.getInitialURL()` into the navigation container itself, so a
// link tapped while the app is killed still arrives as this route's initial
// screen). The desktop-rendered `https://pollis.com/invite/<token>` form needs
// iOS universal links / Android app links (associated-domains + server files)
// before it opens the app — deliberately not configured here.
//
// On failure this renders ONE opaque message for every cause. The Delivery
// Service deliberately cannot tell us whether a token was wrong, expired,
// revoked or used up — distinguishing them would confirm to an attacker that
// a token was real but stale.
export default function InviteLanding() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { token } = useLocalSearchParams<{ token?: string }>();
  const [phase, setPhase] = useState<Phase>("working");

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
        const result = await invoke<RedeemedInvite>(
          "redeem_group_invite_link",
          { token: trimmed, userId },
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
    })();
  }, [token, router, queryClient]);

  const message =
    phase === "working"
      ? "Checking your invite…"
      : phase === "signedOut"
        ? "Sign in to Pollis first, then open the invite link again."
        : phase === "locked"
          ? "Unlock Pollis first, then open the invite link again."
          : "This invite link can't be used. It may be invalid, expired, revoked, or already used up — ask for a new link.";

  return (
    <Screen testID="screen-invite-landing" centered>
      <Crumb segs={[{ label: "POLLIS" }, { label: "Invite", leaf: true }]} />
      <Body>
        <View
          style={{
            paddingHorizontal: 18,
            paddingTop: 24,
            gap: 12,
            alignItems: "center",
          }}
        >
          {phase === "working" ? (
            <ActivityIndicator color={semantic.accent} />
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
      <Ctx cr="POLLIS" name="Group invite" hideBack />
      {phase !== "working" ? (
        <BottomAction>
          <Button
            full
            testID="btn-invite-continue"
            variant="primary"
            onPress={() => router.replace("/")}
          >
            {phase === "failed" ? "BACK TO POLLIS" : "OPEN POLLIS"}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}
