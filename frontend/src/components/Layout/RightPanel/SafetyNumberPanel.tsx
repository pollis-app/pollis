import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { ArrowLeft, ShieldCheck } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import {
  useSafetyNumber,
  useSetContactVerified,
} from "../../../hooks/queries/useUserProfile";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { Button } from "../../ui/Button";
import { LoadingSpinner } from "../../ui/LoaderSpinner";

interface SafetyNumberPanelProps {
  peerUserId: string;
  peerLabel: string;
  onBack: () => void;
}

/** Pair safety number (#1563), one per other member. The QR carries the 60 digits; `qr_payload` (~5 KB) won't fit. */
export const SafetyNumberPanel: React.FC<SafetyNumberPanelProps> = observer(
  ({ peerUserId, peerLabel, onBack }) => {
    const { t } = useTranslation("dms");
    const isTerminal = useSkin() !== "refined";
    const { data: safety, isLoading, isError } = useSafetyNumber(peerUserId);
    const setVerified = useSetContactVerified(peerUserId);
    const gutter = isTerminal ? "px-2.5" : "px-2";
    const title = t("safety.title", { name: peerLabel });

    let body: React.ReactNode;
    if (isLoading) {
      body = (
        <div className="flex justify-center py-6">
          <LoadingSpinner />
        </div>
      );
    } else if (isError || !safety) {
      body = (
        <p className={`${gutter} py-2 text-xs text-muted`}>
          {t("safety.failed")}
        </p>
      );
    } else {
      body = (
        <div
          data-testid="safety-number"
          className={`flex flex-col gap-3 ${gutter} py-3`}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs text-muted">{t("profile.safetyNumber")}</span>
            <span
              data-testid="safety-status"
              className={`text-xs ${
                safety.status === "verified"
                  ? "text-accent"
                  : safety.status === "changed"
                    ? "text-danger"
                    : "text-muted"
              }`}
            >
              {safety.status === "verified"
                ? t("profile.statusVerified")
                : safety.status === "changed"
                  ? t("profile.statusChanged")
                  : t("profile.statusUnverified")}
            </span>
          </div>
          <div className="flex justify-center" data-testid="safety-number-qr">
            <QRCodeSVG
              value={safety.safety_number}
              size={136}
              bgColor="#ffffff"
              fgColor="#000000"
              marginSize={2}
              title={t("safety.qrLabel", { name: peerLabel })}
            />
          </div>
          <code
            dir="ltr"
            data-testid="safety-number-digits"
            className="font-mono font-machine text-sm leading-relaxed text-center break-words text-fg"
          >
            {safety.safety_number}
          </code>
          {safety.status === "changed" && (
            <span className="text-xs text-danger">
              {t("profile.keyChangedWarning")}
            </span>
          )}
          <p className="text-xs text-muted" data-testid="safety-number-shared">
            {t("safety.shared", { name: peerLabel })}
          </p>
          <div>
            <Button
              variant={safety.status === "verified" ? "secondary" : "primary"}
              disabled={setVerified.isPending}
              onClick={() => setVerified.mutate(safety.status !== "verified")}
              data-testid="safety-verify-toggle"
            >
              {safety.status === "verified"
                ? t("profile.removeVerification")
                : t("profile.markVerified")}
            </Button>
          </div>
        </div>
      );
    }

    return (
      <div className="h-full overflow-y-auto" data-testid="safety-panel">
        <div
          className={
            isTerminal
              ? "sticky top-0 z-[1] flex h-bar w-full items-center gap-2 border-b border-line bg-surface px-2.5 text-[0.8rem] text-muted select-none"
              : "flex items-center gap-2 px-2 pt-3 pb-1 text-xs font-medium text-muted"
          }
        >
          <button
            type="button"
            className="icon-btn-sm shrink-0"
            aria-label={t("common:actions.goBack")}
            title={t("common:actions.goBack")}
            data-testid="safety-panel-back"
            onClick={onBack}
          >
            <ArrowLeft size={14} className="rtl-mirror" aria-hidden="true" />
          </button>
          <ShieldCheck size={12} aria-hidden="true" />
          <bdi className="min-w-0 flex-1 truncate" data-testid="safety-panel-title">
            {title}
          </bdi>
        </div>
        {body}
      </div>
    );
  },
);
