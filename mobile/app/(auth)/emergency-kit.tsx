import { useEffect, useRef, useState } from "react";
import { View, Text, Pressable, ScrollView, Share } from "react-native";
import * as Clipboard from "expo-clipboard";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  Card,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts, r } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

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
 * COPY and SAVE below mirror the desktop `SaveSecretKeyScreen` affordances.
 */

// How long a copy outcome stays on the button before returning to idle.
// Matches CreatedInviteLinkCard, the other once-only-secret surface.
const COPY_FEEDBACK_MS = 2000;

type CopyState = "idle" | "copied" | "failed";

/**
 * The emergency-kit document, byte-for-byte the same text desktop writes to
 * `pollis-emergency-kit-*.txt` (`auth:emergencyKit.document`). Whatever the
 * user saves on a phone should be the same artifact they would have saved on
 * a laptop — a kit that reads differently per platform is a support problem
 * the day someone compares them.
 */
function emergencyKitDocument(secretKey: string): string {
  return `POLLIS — EMERGENCY KIT
======================

Your Secret Key is the only way to recover access to your account
from a new device when you don't have any other Pollis device with
you. Treat it like a master password.

If you lose this key AND lose access to all of your devices,
your account is unrecoverable. Pollis cannot reset it for you.

  SECRET KEY:

    ${secretKey}

Store this file somewhere safe (a password manager, encrypted
backup, or printed and locked away). Anyone with this key + your
email address can sign in as you on a new device.

Generated: ${new Date().toISOString()}
`;
}

function EmergencyKit() {
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

  return (
    <Screen testID="screen-auth-emergency-kit" centered>
      <Crumb
        segs={[{ label: "AUTH" }, { label: "Emergency kit", leaf: true }]}
      />
      <Body>
        <ScrollView contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 18, gap: 18, paddingBottom: 24 }}>
          <View style={{ gap: 8 }}>
            <Text style={[ty.h1, { color: semantic.ink }]}>Save your recovery key</Text>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                lineHeight: 19,
                color: semantic.mute,
              }}
            >
              This key is the only way to add Pollis to a new device or
              recover after losing this one. We don't store it anywhere
              you can read it later — write it down or save it to a
              password manager now.
            </Text>
          </View>

          <Card style={{ borderColor: semantic.accent }}>
            <Text
              selectable
              style={{
                fontFamily: fonts.mono400,
                fontSize: 14,
                lineHeight: 22,
                color: semantic.accent,
                letterSpacing: 0.4,
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
                variant={copyState === "failed" ? "danger" : "primary"}
                onPress={onCopy}
                icon={
                  copyState === "copied" ? (
                    <Icon.check color="#0a0907" />
                  ) : copyState === "failed" ? (
                    <Icon.alert color={semantic.danger} />
                  ) : (
                    <Icon.copy color="#0a0907" />
                  )
                }
              >
                {copyState === "copied"
                  ? "COPIED"
                  : copyState === "failed"
                    ? "COPY FAILED"
                    : "COPY"}
              </Button>
            </View>
            <View style={{ flex: 1 }}>
              <Button
                full
                testID="btn-share-recovery-key"
                onPress={onShare}
                icon={<Icon.share color={semantic.ink} />}
              >
                SAVE
              </Button>
            </View>
          </View>

          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
            }}
          >
            <Icon.shield color={semantic.mute} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 11,
                color: semantic.mute,
                flex: 1,
                lineHeight: 16,
              }}
            >
              Anyone with this key can sign in as you on a new device.
              Treat it like a master password.
            </Text>
          </View>

          <Pressable
            onPress={() => setAcknowledged((v) => !v)}
            testID="toggle-recovery-ack"
            accessibilityRole="checkbox"
            accessibilityLabel="I've saved my recovery key"
            accessibilityState={{ checked: acknowledged }}
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 10,
              paddingVertical: 8,
            }}
          >
            <View
              style={{
                width: 18,
                height: 18,
                borderWidth: 1,
                borderColor: acknowledged ? semantic.accent : semantic.hairStrong,
                backgroundColor: acknowledged ? semantic.accent : "transparent",
                borderRadius: r.sm,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              {acknowledged ? <Icon.check color="#0a0907" /> : null}
            </View>
            <Text
              style={{
                flex: 1,
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.ink,
              }}
            >
              I've saved my recovery key in a safe place.
            </Text>
          </Pressable>
        </ScrollView>
      </Body>
      <BottomAction>
        <Button
          testID="btn-continue"
          full
          variant="primary"
          onPress={onContinue}
          disabled={!acknowledged}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          CONTINUE
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(EmergencyKit);
