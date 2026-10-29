import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { QRCodeSVG } from "qrcode.react";
import { Button } from "../ui/Button";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { AuthError } from "./AuthError";
import * as api from "../../services/api";
import { errorMessage } from "../../utils/errorMessage";

/// Codes shown unattended before asking (each lives two minutes).
const AUTO_REFRESHES = 5;

interface OfferCodeProps {
  deviceName: string;
  onSignedIn: (result: api.AuthResult) => void | Promise<void>;
}

/** QR for a signed-in phone to scan (#1361): this device's ephemeral key and a link id, never a secret. */
export const OfferCode: React.FC<OfferCodeProps> = ({ deviceName, onSignedIn }) => {
  const { t } = useTranslation("auth");
  const [handle, setHandle] = useState<api.DeviceOfferHandle | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stopped, setStopped] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  // Bumped per code and on unmount, so late answers are ignored.
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
            {/* Dark on accent: themed, still normal polarity for decoders. */}
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
          {/* For reading by hand. */}
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
