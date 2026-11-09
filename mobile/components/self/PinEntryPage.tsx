import { useEffect, useState } from "react";
import { View } from "react-native";
import { Screen, Header } from "../ui";
import { Heading } from "../auth/Heading";
import { PinBusy, PinCells, PinKeypad } from "../auth/PinPad";
import { ErrorText } from "./SettingsField";
import { useIsRegular } from "../../hooks/useLayoutClass";

/**
 * A settings page whose one input is a 4-digit PIN (change PIN, turning on biometric unlock):
 * the heading, the dots, the keypad. The fourth digit calls `onComplete`; while `busy` the
 * loading line takes the keypad's place (no frozen pad). A new `resetKey` clears the digits.
 */
export function PinEntryPage({
  testID,
  title,
  backTo,
  heading,
  subtitle,
  step,
  error,
  errorTestID,
  busy,
  busyLabel,
  resetKey,
  onComplete,
}: {
  testID: string;
  title: string;
  backTo: string;
  heading: string;
  subtitle?: string;
  step?: string;
  error?: string | null;
  errorTestID?: string;
  busy: boolean;
  busyLabel: string;
  resetKey: unknown;
  onComplete: (pin: string) => void;
}) {
  const regular = useIsRegular();
  const [pin, setPin] = useState("");

  useEffect(() => {
    setPin("");
  }, [resetKey]);

  const pushDigit = (d: string) => {
    if (busy || pin.length >= 4) {
      return;
    }
    const next = pin + d;
    setPin(next);
    if (next.length === 4) {
      onComplete(next);
    }
  };

  // The keypad is the only input: no keyboard padding can apply (#1642).
  return (
    <Screen testID={testID} avoidKeyboard={false}>
      <Header title={title} backTo={backTo} />
      <View style={regular ? { flex: 1, justifyContent: "center", gap: 32 } : { flex: 1 }}>
        <View style={{ flex: regular ? 0 : 1, paddingHorizontal: 24, paddingTop: regular ? 0 : 32, gap: 32 }}>
          <Heading testID={`${testID}-heading`} step={step} title={heading} subtitle={subtitle} />
          {busy ? null : (
            <View style={{ gap: 14 }}>
              <PinCells length={pin.length} />
              {error ? (
                <View style={{ alignSelf: "center" }}>
                  <ErrorText testID={errorTestID}>{error}</ErrorText>
                </View>
              ) : null}
            </View>
          )}
        </View>
        {busy ? (
          <View style={{ flex: regular ? 0 : 1, justifyContent: "center" }}>
            <PinBusy label={busyLabel} />
          </View>
        ) : (
          <PinKeypad onDigit={pushDigit} onBackspace={() => setPin(pin.slice(0, -1))} />
        )}
      </View>
    </Screen>
  );
}
