import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { QRCodeSVG } from "qrcode.react";
import { Button } from "../ui/Button";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { AuthError } from "./AuthError";
import * as api from "../../services/api";
import { errorMessage } from "../../utils/errorMessage";

/// How many codes in a row this screen shows on its own before it stops and
/// asks. Each lives two minutes, so this is about ten minutes of an unattended
/// sign-in screen; after that a person has to ask for another.
const AUTO_REFRESHES = 5;

interface OfferCodeProps {
  deviceName: string;
  /// A signed-in device accepted the code: this device is signed in and must
  /// now enroll (the scanning device approves it).
  onSignedIn: (result: api.AuthResult) => void | Promise<void>;
}

/**
 * The QR this NEW device shows for a signed-in phone to scan (#1361) — the
 * reverse of #1207. It carries this device's ephemeral public key and a link
 * id, never a secret: the phone wraps the account key to exactly the key it
 * reads here, and this device collects its session with a poll secret the QR
 * does not carry. Waiting is one awaited Rust call per code; a code that lapses
 * is replaced, up to `AUTO_REFRESHES`.
 */
export const OfferCode: React.FC<OfferCodeProps> = ({ deviceName, onSignedIn }) => {
  const { t } = useTranslation("auth");
  const [handle, setHandle] = useState<api.DeviceOfferHandle | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stopped, setStopped] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  // Bumped on every new code and on unmount: a late answer for a superseded
  // code must not steer the screen.
  const generation = useRef(0);
  const refreshes = useRef(0);

  const show = async () => {
    generation.current += 1;
    const mine = generation.current;
    setError(null);
    setStopped(false);
    try {
      const next = await api.createDeviceOffer(deviceName);
      if (generation.current !== mine) {
        return;
      }
      setHandle(next);
      const outcome = await api.awaitDeviceOffer(next.link_id);
      if (generation.current !== mine) {
        return;
      }
      if (outcome.state === "signed_in") {
        setSigningIn(true);
        await onSignedIn(outcome.result);
        return;
      }
      if (refreshes.current < AUTO_REFRESHES) {
        refreshes.current += 1;
        void show();
      } else {
        setStopped(true);
      }
    } catch (err) {
      if (generation.current === mine) {
        setSigningIn(false);
        setError(errorMessage(err) || t("link.qrFailed"));
      }
    }
  };

  useEffect(() => {
    void show();
    return () => {
      generation.current += 1;
      void api.cancelDeviceOffer().catch(() => undefined);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const again = () => {
    refreshes.current = 0;
    void show();
  };

  return (
    <div data-testid="link-offer" className="flex flex-col gap-4">
      <p className="text-sm text-muted leading-relaxed">{t("link.qrIntro")}</p>
      {error && <AuthError message={error} />}
      {stopped || error ? (
        <div>
          <Button data-testid="link-offer-refresh" variant="secondary" onClick={again}>
            {t("link.qrRefresh")}
          </Button>
        </div>
      ) : handle ? (
        <>
          <div className="self-start p-3 rounded-lg bg-accent" data-testid="link-offer-qr">
            {/* Dark modules on the accent: themed, and still normal polarity
                (dark on light), which every camera decoder reads. */}
            <QRCodeSVG
              value={handle.qr_payload}
              size={208}
              bgColor="var(--c-accent)"
              fgColor="var(--c-bg)"
              marginSize={2}
              level="M"
              title={t("link.qrAlt")}
            />
          </div>
          {/* For a code read by hand on another computer; selectable, but
              never needed when a phone scans it. */}
          <code
            data-testid="link-offer-payload"
            className="font-mono text-xs text-dim break-all select-all leading-relaxed"
          >
            {handle.qr_payload}
          </code>
          <p data-testid="link-offer-status" className="flex items-center gap-2 text-xs font-mono text-muted">
            <LoadingSpinner size="sm" />
            {signingIn ? t("link.signingIn") : t("link.qrWaiting")}
          </p>
        </>
      ) : (
        <LoadingSpinner size="sm" />
      )}
    </div>
  );
};
