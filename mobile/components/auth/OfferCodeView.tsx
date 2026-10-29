import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import { QrCode } from "../QrCode";
import { AuthError } from "./AuthError";
import { semantic, type as ty, fonts, r, space } from "../../theme/tokens";
import { awaitDeviceOffer, cancelDeviceOffer, createDeviceOffer, type DeviceOfferHandle } from "../../hooks/queries";

// Codes shown in a row before it asks; each lives two minutes, so about ten
// minutes of an unattended sign-in screen.
const AUTO_REFRESHES = 5;

// Lets a long unbroken code wrap at any character.
function breakAnywhere(text: string): string {
  return Array.from(text).join("​");
}

/**
 * The code this NEW phone shows for a signed-in device to scan (#1361). It
 * carries this device's ephemeral public key and a link id, never a secret.
 */
export function OfferCodeView({ deviceName, onSignedIn }: { deviceName: string; onSignedIn: () => void }) {
  const { t } = useTranslation("auth");
  const [handle, setHandle] = useState<DeviceOfferHandle | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stopped, setStopped] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const generation = useRef(0);
  const refreshes = useRef(0);

  const show = async () => {
    generation.current += 1;
    const mine = generation.current;
    setError(null);
    setStopped(false);
    try {
      const next = await createDeviceOffer(deviceName);
      if (generation.current !== mine) {
        return;
      }
      setHandle(next);
      const outcome = await awaitDeviceOffer(next.link_id);
      if (generation.current !== mine) {
        return;
      }
      if (outcome.state === "signed_in") {
        setSigningIn(true);
        onSignedIn();
        return;
      }
      if (refreshes.current < AUTO_REFRESHES) {
        refreshes.current += 1;
        void show();
      } else {
        setStopped(true);
      }
    } catch (e) {
      if (generation.current === mine) {
        setError((e as Error).message || t("link.qrFailed"));
      }
    }
  };

  useEffect(() => {
    void show();
    return () => {
      generation.current += 1;
      void cancelDeviceOffer().catch(() => undefined);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (error || stopped) {
    return (
      <View style={{ gap: space.lg }}>
        {error ? <AuthError testID="link-offer-error" message={error} /> : null}
        <Button
          testID="btn-link-offer-refresh"
          variant="secondary"
          full
          onPress={() => {
            refreshes.current = 0;
            void show();
          }}
        >
          {t("link.qrRefresh")}
        </Button>
      </View>
    );
  }

  if (!handle) {
    return <ActivityIndicator color={semantic.accent} />;
  }

  return (
    <View testID="link-offer" style={{ gap: space.xxl }}>
      <View style={{ alignItems: "center" }} accessible accessibilityLabel={t("link.qrAlt")}>
        <QrCode testID="link-offer-qr" value={handle.qr_payload} size={232} frame={space.lg} radius={r.lg} />
      </View>
      {/* For entering by hand on a phone whose camera can't read it. */}
      <Text
        testID="link-offer-payload"
        selectable
        style={{ fontFamily: fonts.mono400, fontSize: 13, lineHeight: 20, color: semantic.dim }}
      >
        {breakAnywhere(handle.qr_payload)}
      </Text>
      <View
        style={{ flexDirection: "row", alignItems: "center", justifyContent: "center", gap: space.md }}
        accessibilityLiveRegion="polite"
      >
        <ActivityIndicator color={semantic.accent} />
        <Text testID="link-offer-status" style={ty.secondary}>
          {signingIn ? t("link.signingIn") : t("link.qrWaiting")}
        </Text>
      </View>
    </View>
  );
}
