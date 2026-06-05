import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { ShieldAlert, X } from "lucide-react";
import { observer } from "mobx-react-lite";
import { keyChangeStore } from "../../stores/keyChangeStore";

interface KeyChangeBannerProps {
  /// User id of the peer whose key has changed. Pass the DM's `user2_id`
  /// for 1:1 DMs. Returns `null` when there is nothing to surface so the
  /// caller can render this unconditionally and rely on the banner to
  /// no-op when there's no pending warning.
  peerUserId: string | null | undefined;
  peerLabel?: string;
}

/// Inline "this contact's identity key changed" banner. Rendered at the
/// top of an open conversation (DM page) the moment the backend emits a
/// `key_changed` realtime event. Policy is ADVISORY-with-acknowledge —
/// the banner is dismissable and sends are unaffected; users are nudged
/// to re-verify out-of-band via the profile page.
///
/// Not a modal — this is an inline banner inside the conversation chrome.
export const KeyChangeBanner: React.FC<KeyChangeBannerProps> = observer(({
  peerUserId,
  peerLabel,
}) => {
  const navigate = useNavigate();
  const flagged = peerUserId ? keyChangeStore.flagged[peerUserId] : undefined;
  const acknowledge = keyChangeStore.acknowledge;

  if (!peerUserId || !flagged) {
    return null;
  }

  const name = peerLabel ?? "this contact";
  return (
    <div
      data-testid="key-change-banner"
      role="alert"
      className="flex items-start gap-2 px-4 py-2 text-xs font-mono"
      style={{
        borderBottom: "1px solid var(--c-border)",
        background: "rgba(240, 180, 41, 0.08)",
        color: "#f0b429",
      }}
    >
      <ShieldAlert size={16} aria-hidden="true" style={{ flexShrink: 0, marginTop: 1 }} />
      <div className="flex-1 min-w-0">
        <span style={{ color: "#f0b429", fontWeight: 600 }}>
          Safety number changed
        </span>
        <span style={{ color: "var(--c-text-muted)" }}>
          {" "}— {name}'s identity key is different from what was pinned. Verify
          out-of-band before trusting this conversation.
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
        Verify
      </button>
      <button
        type="button"
        data-testid="key-change-banner-dismiss"
        onClick={() => acknowledge(peerUserId)}
        aria-label="Dismiss safety number warning"
        className="icon-btn-sm"
        style={{ flexShrink: 0, color: "inherit" }}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
});
