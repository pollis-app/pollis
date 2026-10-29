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

/**
 * Emergency Kit — shown once, right after a brand-new account's PIN is set.
 * The recovery key from `verify_otp` lives in the store (`pendingSecretKey`)
 * for this one screen and is dropped once the user acknowledges saving it;
 * then the same /(auth)/initializing path a returning user takes.
 *
 * The key is unrecoverable afterwards, so Copy and Save are real ways off this
 * screen (desktop's `SaveSecretKeyScreen`), not just `selectable` text.
 */

// How long a copy outcome stays on the button before returning to idle.
// Matches CreatedInviteLinkCard, the other once-only-secret surface.
const COPY_FEEDBACK_MS = 2000;

type CopyState = "idle" | "copied" | "failed";

/**
 * The emergency-kit document, byte-for-byte the text desktop writes to
 * `pollis-emergency-kit-*.txt` (the shared `auth:emergencyKit.document`
 * catalogue entry), so a kit reads the same on every platform.
 */
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

  // Clear the outcome a couple of seconds after it appears, and never leave a
  // timer behind that would set state on an unmounted screen.
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

  // Defensive: if someone deep-links here without a stashed key, just
  // continue to initializing — nothing to display.
  if (!pendingSecretKey) {
    router.replace("/(auth)/initializing");
    return null;
  }

  const keyGroups = pendingSecretKey.split("-");

  const onContinue = () => {
    setPendingSecretKey(null);
    router.replace("/(auth)/initializing");
  };

  // VERIFIED copy: a rejected or false `setStringAsync` shows as a failure.
  // Deliberately no read-back: on iOS 14+ that fires the "pasted from Pollis"
  // banner every tap.
  const onCopy = async () => {
    // Back to idle first so a second tap on an already-failed button is a
    // visible state change, and so the feedback timer restarts.
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(pendingSecretKey).catch(
      () => false,
    );
    haptic(copied ? "success" : "error");
    setCopyState(copied ? "copied" : "failed");
  };

  // Hand the kit to the OS share sheet as `message`, NOT a file: a file would
  // sit in the app sandbox first, and this is the most sensitive string the app
  // holds (#1001). Dismissing the sheet is a normal outcome.
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
            {/* One Text per hyphen group so a line only ever breaks between
                groups — Android's breaker otherwise strands a lone "-86394".
                Copy and Save below carry the exact string. */}
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
          {/* The copy outcome, announced: the button label alone changes
              silently for a screen reader. */}
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

        {/* The acknowledgement: one full-width checkbox row (the shared
            CheckRow — a clearly visible box, accent with a check when on). */}
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
