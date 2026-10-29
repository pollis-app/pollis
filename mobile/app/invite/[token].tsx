import { useEffect, useRef, useState } from "react";
import { View, Text, ActivityIndicator } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { invoke } from "../../lib/native";
import { restoreSession } from "../../hooks/queries/useAuth";
import { appStore } from "../../stores/appStore";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { groupQueryKeys, type RedeemedInvite } from "../../hooks/queries";
import { useThemeRerender } from "../../components/theme";

interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

type Phase = "working" | "confirm" | "joining" | "signedOut" | "locked" | "failed";

// #847 invite link landing. #1094: arriving does NOT join; one tap is never enough, and every failure shows ONE opaque message.
export default function InviteLanding() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const { openGroup } = useOpenConversation();
  const queryClient = useQueryClient();
  const { token } = useLocalSearchParams<{ token?: string }>();
  const [phase, setPhase] = useState<Phase>("working");
  const [readyUserId, setReadyUserId] = useState<string | null>(null);

  // A cold start bypasses app/index.tsx: restore the session first.
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
      // A new group just became visible: invalidate broadly, like desktop.
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.all });
      openGroup(result.group_id, { replace: true });
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
    <Screen testID="screen-invite-landing">
      <Header title={t("mobile:invite.groupInvite")} hideBack />
      <Body contentContainerStyle={{ padding: space.xxl }}>
        <View
          accessible
          accessibilityLiveRegion="polite"
          style={{ paddingTop: space.xxl, gap: space.lg, alignItems: "center" }}
        >
          {phase === "working" || phase === "joining" ? (
            <ActivityIndicator color={semantic.accent} />
          ) : null}
          {phase === "failed" ? <Icon.alert size={24} color={semantic.accent} /> : null}
          {phase === "confirm" ? (
            <Text
              testID="invite-confirm-title"
              accessibilityRole="header"
              style={[ty.title, { textAlign: "center" }]}
            >
              {t("mobile:invite.confirmTitle")}
            </Text>
          ) : null}
          <Text
            testID="invite-landing-message"
            style={[ty.body, { color: semantic.dim, textAlign: "center" }]}
          >
            {message}
          </Text>
        </View>
      </Body>
      {phase === "confirm" ? (
        <BottomAction>
          <Button
            full
            testID="btn-invite-join"
            variant="primary"
            onPress={join}
          >
            {t("mobile:invite.join")}
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
              ? t("mobile:invite.backToPollis")
              : t("mobile:invite.openPollis")}
          </Button>
        </BottomAction>
      )}
    </Screen>
  );
}
