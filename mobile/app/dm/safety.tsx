import { View, Text } from "react-native";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { Screen, Header, Body, Card, Chip, Txt } from "../../components/ui";
import { QrCode } from "../../components/QrCode";
import { Icon } from "../../components/icons";
import { semantic, fonts, space } from "../../theme/tokens";
import { invoke } from "../../lib/native";
import { useSafetyNumber, useSetContactVerified } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { useThemeRerender } from "../../components/theme";

interface RawProfile {
  id: string;
  username?: string;
  preferred_name?: string;
}

// "You & <name>" (#1563): the safety number of a PAIR, so it lives in the DM.
function DMSafetyNumber() {
  useThemeRerender();
  const { t } = useTranslation("dms");
  const router = useNav();
  const { peerId: peerParam, name: nameParam } = useRouteParams<{
    peerId?: string;
    name?: string;
  }>();
  const peerId = peerParam ?? null;
  const isSelf = appStore.currentUser?.id === peerId;

  const profile = useQuery({
    queryKey: ["user", "profile", peerId],
    queryFn: async (): Promise<RawProfile | null> => {
      if (!peerId) {
        return null;
      }
      return await invoke<RawProfile | null>("get_user_profile", {
        userId: peerId,
      });
    },
    enabled: !!peerId && !nameParam,
    staleTime: 1000 * 60,
  });

  const safety = useSafetyNumber(isSelf ? null : peerId);
  const setVerified = useSetContactVerified();

  const name = nameParam
    ? nameParam
    : profile.data?.preferred_name ||
      (profile.data?.username
        ? `@${profile.data.username}`
        : t("profile.fallbackName"));
  const verification = safety.data?.status;

  const onToggleVerified = () => {
    if (!peerId || !safety.data) {
      return;
    }
    setVerified.mutate({
      peerUserId: peerId,
      verified: safety.data.status !== "verified",
    });
  };

  return (
    <Screen testID="screen-dm-safety" aboveTabBar={router.inPane}>
      <Header
        onBack={router.onBack}
        title={t("safety.title", { name })}
        backTo={t("mobile:conversationInfo.info")}
      />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xl }}>
        {safety.isLoading ? (
          <Txt variant="secondary">{t("mobile:user.computing")}</Txt>
        ) : safety.isError ? (
          <Txt variant="secondary">
            {(safety.error as Error).message || t("safety.failed")}
          </Txt>
        ) : safety.data ? (
          <Card style={{ gap: space.lg, alignItems: "center" }}>
            {/* The QR carries the digits: `qr_payload` (~5 KB) is too big. */}
            <View accessible accessibilityLabel={t("safety.qrLabel", { name })}>
              <QrCode
                testID="img-safety-qr"
                value={safety.data.safety_number}
                size={176}
              />
            </View>
            <Text
              selectable
              testID="text-safety-number"
              style={{
                fontFamily: fonts.mono400,
                fontSize: 16,
                lineHeight: 26,
                color: semantic.text,
                letterSpacing: 0.6,
                textAlign: "center",
                writingDirection: "ltr",
              }}
            >
              {safety.data.safety_number}
            </Text>
            <View
              style={{
                flexDirection: "row",
                flexWrap: "wrap",
                alignItems: "center",
                justifyContent: "center",
                gap: space.sm,
              }}
            >
              <Chip
                testID="btn-verify"
                selected={verification === "verified"}
                variant="outline"
                onPress={onToggleVerified}
                disabled={setVerified.isPending}
                accessibilityLabel={
                  verification === "verified"
                    ? t("profile.removeVerification")
                    : t("profile.markVerified")
                }
              >
                {setVerified.isPending
                  ? t("mobile:common.working")
                  : verification === "verified"
                    ? t("mobile:user.verified")
                    : t("profile.markVerified")}
              </Chip>
            </View>
            {verification === "changed" ? (
              <View
                style={{
                  flexDirection: "row",
                  alignItems: "center",
                  gap: space.xs,
                }}
              >
                <Icon.alert size={16} color={semantic.accent} />
                <Txt variant="secondary" style={{ flex: 1, color: semantic.text }}>
                  {t("mobile:user.keyChanged")}
                </Txt>
              </View>
            ) : null}
          </Card>
        ) : null}
        <Txt variant="secondary" testID="text-safety-shared">
          {t("safety.shared", { name })}
        </Txt>
      </Body>
    </Screen>
  );
}

export default observer(DMSafetyNumber);
