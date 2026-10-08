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

/**
 * Emergency Kit — shown once, right after a brand-new account's PIN is set.
 * The recovery key emitted by `verify_otp` lives in the MobX store
 * (`pendingSecretKey`) for one screen-jump only. The user must explicitly
 * acknowledge they've saved it before we drop it from memory.
 *
 * After ACK we route to /(auth)/initializing — exactly the same path a
 * returning user takes — so the rest of the launch sequence stays the
 * same regardless of whether this was a new account or not.
 *
 * The key is shown exactly once and is unrecoverable afterwards, so it needs
 * real ways OFF this screen. `selectable` text alone was not one: it is an
 * undiscoverable long-press, it selects a 40-odd character mono string by
 * hand, and it is the single worst string in the product to mis-transcribe.
 * Copy and Save below mirror the desktop `SaveSecretKeyScreen` affordances.
 */

// How long a copy outcome stays on the button before returning to idle.
// Matches CreatedInviteLinkCard, the other once-only-secret surface.
const COPY_FEEDBACK_MS = 2000;

type CopyState = "idle" | "copied" | "failed";

/**
 * The emergency-kit document, byte-for-byte the same text desktop writes to
 * `pollis-emergency-kit-*.txt` — the shared `auth:emergencyKit.document`
 * catalogue entry. Whatever the user saves on a phone should be the same
 * artifact they would have saved on a laptop — a kit that reads differently
 * per platform is a support problem the day someone compares them.
 */
function emergencyKitDocument(secretKey: string): string {
  return i18n.t("auth:emergencyKit.document", {
    secretKey,
    generated: new Date().toISOString(),
  });
}

function EmergencyKit() {
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

  const onContinue = () => {
    setPendingSecretKey(null);
    router.replace("/(auth)/initializing");
  };

  // VERIFIED copy (#897/#958 semantics): `setStringAsync` resolves to a
  // boolean, and a rejected or false write shows as a visible failure. A key
  // that is shown once and silently failed to copy is the worst possible thing
  // to report as success. We deliberately do NOT read the clipboard back —
  // on iOS 14+ that fires the system "pasted from Pollis" banner every tap.
  const onCopy = async () => {
    // Back to idle first so a second tap on an already-failed button is a
    // visible state change, and so the feedback timer restarts.
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(pendingSecretKey).catch(
      () => false,
    );
    setCopyState(copied ? "copied" : "failed");
  };

  // Hand the whole kit to the OS share sheet — password manager, Notes, Mail,
  // AirDrop to a laptop. Deliberately passed as `message`, NOT written to a
  // file first: #1001 closed the plaintext-on-disk leaks and staged pasted
  // attachments in memory rather than through a temp file, and the recovery
  // key is the most sensitive string the app ever holds. Desktop can write a
  // .txt because it hands it straight to the OS download flow; here a file
  // would have to sit in the app sandbox first, so this stays in memory.
  // Dismissing the sheet is a normal outcome, not an error state.
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
            <Text
              selectable
              style={{
                fontFamily: fonts.mono400,
                fontSize: 17,
                lineHeight: 26,
                color: semantic.text,
              }}
            >
              {pendingSecretKey}
            </Text>
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
