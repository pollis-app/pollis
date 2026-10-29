import { useEffect, useRef, useState } from "react";
import { View, Text, Share } from "react-native";
import * as Clipboard from "expo-clipboard";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Body, Card, Button, BottomAction, CheckRow } from "../../components/ui";
import { Icon } from "../../components/icons";
import { Heading } from "../../components/auth/Heading";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import i18n from "../../i18n";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

const COPY_FEEDBACK_MS = 2000;

type CopyState = "idle" | "copied" | "failed";

/** Byte-for-byte the text desktop writes (`auth:emergencyKit.document`). */
function emergencyKitDocument(secretKey: string): string {
  return i18n.t("auth:emergencyKit.document", {
    secretKey,
    generated: new Date().toISOString(),
  });
}

function EmergencyKit() {
  useThemeRerender();
  const { t } = useTranslation("auth");
  const router = useRouter();
  const pendingSecretKey = appStore.pendingSecretKey;
  const setPendingSecretKey = appStore.setPendingSecretKey;
  const [acknowledged, setAcknowledged] = useState(false);
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

  if (!pendingSecretKey) {
    router.replace("/(auth)/initializing");
    return null;
  }

  const keyGroups = pendingSecretKey.split("-");

  const onContinue = () => {
    setPendingSecretKey(null);
    router.replace("/(auth)/initializing");
  };

  // VERIFIED copy (#897): a false or rejected write shows as failure. Never read back (iOS paste banner).
  const onCopy = async () => {
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(pendingSecretKey).catch(
      () => false,
    );
    haptic(copied ? "success" : "error");
    setCopyState(copied ? "copied" : "failed");
  };

  // Passed as `message`, never a file: the recovery key must not touch disk (#1001).
  const onShare = () => {
    Share.share({ message: emergencyKitDocument(pendingSecretKey) }).catch(
      () => {},
    );
  };

  const copyLabel =
    copyState === "copied"
      ? t("secretKey.copied")
      : copyState === "failed"
        ? t("mobile:auth.emergencyKit.copyFailed")
        : t("common:actions.copy");

  return (
    <Screen testID="screen-auth-emergency-kit" centered>
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 52, gap: 24 }}>
        <Heading
          title={t("mobile:auth.emergencyKit.title")}
          subtitle={t("mobile:auth.emergencyKit.intro")}
        />

        <View style={{ gap: 12 }}>
          <Card>
            {/* One Text per hyphen group so lines break only between groups. */}
            <View
              accessible
              accessibilityLabel={pendingSecretKey}
              style={{ flexDirection: "row", flexWrap: "wrap" }}
            >
              {keyGroups.map((g, i) => (
                <Text
                  key={i}
                  style={{
                    fontFamily: fonts.mono400,
                    fontSize: 17,
                    lineHeight: 26,
                    color: semantic.text,
                  }}
                >
                  {i < keyGroups.length - 1 ? `${g}-` : g}
                </Text>
              ))}
            </View>
          </Card>

          <View style={{ flexDirection: "row", gap: 8 }}>
            <View style={{ flex: 1 }}>
              <Button
                full
                testID="btn-copy-recovery-key"
                variant="secondary"
                onPress={onCopy}
                icon={
                  copyState === "copied" ? (
                    <Icon.check size={18} color={semantic.text} />
                  ) : copyState === "failed" ? (
                    <Icon.alert size={18} color={semantic.danger} />
                  ) : (
                    <Icon.copy size={18} color={semantic.text} />
                  )
                }
              >
                {copyLabel}
              </Button>
            </View>
            <View style={{ flex: 1 }}>
              <Button
                full
                testID="btn-share-recovery-key"
                variant="secondary"
                onPress={onShare}
                icon={<Icon.share size={18} color={semantic.text} />}
              >
                {t("common:actions.save")}
              </Button>
            </View>
          </View>
          {/* Announced: the label change alone is silent for a screen reader. */}
          {copyState !== "idle" ? (
            <Text
              accessibilityLiveRegion="polite"
              style={[ty.secondary, { textAlign: "center" }]}
            >
              {copyLabel}
            </Text>
          ) : null}
        </View>

        <View style={{ flexDirection: "row", alignItems: "flex-start", gap: 10 }}>
          <View style={{ paddingTop: 2 }}>
            <Icon.shield size={18} color={semantic.dim} />
          </View>
          <Text style={[ty.secondary, { flex: 1 }]}>
            {t("mobile:auth.emergencyKit.warning")}
          </Text>
        </View>

        <CheckRow
          testID="toggle-recovery-ack"
          checked={acknowledged}
          onPress={() => setAcknowledged((v) => !v)}
          label={t("mobile:auth.emergencyKit.ackText")}
          accessibilityLabel={t("mobile:auth.emergencyKit.ackLabel")}
        />
      </Body>
      <BottomAction>
        <Button
          testID="btn-continue"
          full
          variant="primary"
          onPress={onContinue}
          disabled={!acknowledged}
          iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
        >
          {t("pinCreate.continue")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(EmergencyKit);
