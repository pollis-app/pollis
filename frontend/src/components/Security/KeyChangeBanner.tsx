import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { ShieldAlert, X } from "lucide-react";
import { observer } from "mobx-react-lite";
import { keyChangeStore } from "../../stores/keyChangeStore";

interface KeyChangeBannerProps {
  /// For 1:1 DMs the `user2_id`; renders nothing when there is no warning.
  peerUserId: string | null | undefined;
  peerLabel?: string;
}

/// Inline, dismissable "identity key changed" banner; advisory, sends unaffected.
export const KeyChangeBanner: React.FC<KeyChangeBannerProps> = observer(({
  peerUserId,
  peerLabel,
}) => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const flagged = peerUserId ? keyChangeStore.flagged[peerUserId] : undefined;
  const acknowledge = keyChangeStore.acknowledge;

  if (!peerUserId || !flagged) {
    return null;
  }

  const name = peerLabel ?? t("keyChange.defaultName");
  return (
    <div
      data-testid="key-change-banner"
      role="alert"
      className="flex items-start gap-2 px-4 py-2 text-xs font-mono border-b border-line"
      style={{
        background: "rgba(240, 180, 41, 0.08)",
        color: "#f0b429",
      }}
    >
      <ShieldAlert size={16} aria-hidden="true" style={{ flexShrink: 0, marginTop: 1 }} />
      <div className="flex-1 min-w-0">
        <span style={{ color: "#f0b429", fontWeight: 600 }}>
          {t("keyChange.heading")}
        </span>
        <span className="text-muted">
          {" "}
          {t("keyChange.body", { name })}
        </span>
      </div>
      <button
        type="button"
        data-testid="key-change-banner-verify"
        onClick={() => navigate({ to: "/user/$userId", params: { userId: peerUserId } })}
        className="font-mono"
        style={{
          background: "none",
          border: "1px solid currentColor",
          padding: "1px 6px",
          borderRadius: 3,
          color: "inherit",
          cursor: "pointer",
          fontSize: "inherit",
          flexShrink: 0,
        }}
      >
        {t("keyChange.verifyButton")}
      </button>
      <button
        type="button"
        data-testid="key-change-banner-dismiss"
        onClick={() => acknowledge(peerUserId)}
        aria-label={t("keyChange.dismissLabel")}
        className="icon-btn-sm"
        style={{ flexShrink: 0, color: "inherit" }}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
});
