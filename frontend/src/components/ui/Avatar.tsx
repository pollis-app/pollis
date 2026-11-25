import React from "react";
import { useTranslation } from "react-i18next";
import { User } from "lucide-react";
import { useAvatarBlobUrl } from "../../hooks/queries/useUserProfile";
import type { PresenceStatus } from "../../stores/presenceStore";

interface AvatarProps {
  avatarKey?: string | null;
  size?: number;
  alt?: string;
  testId?: string;
  variant?: "list" | "profile";
  // Optional corner presence dot (#132); omitted by default.
  presence?: PresenceStatus;
}

const PRESENCE_COLORS: Record<PresenceStatus, string> = {
  online: "var(--c-accent)",
  // #2110: the online dot with a bite taken out of it, a crescent moon.
  away: "var(--c-accent)",
  offline: "var(--c-bg)",
};

const RING = "var(--c-surface, var(--c-bg))";

/** The bite that turns the dot into a crescent (#2110): a circle in the ring's colour, clipped by the dot. */
const AWAY_BITE: React.CSSProperties = {
  position: "absolute",
  top: "-20%",
  left: "-20%",
  width: "80%",
  height: "80%",
  borderRadius: "50%",
  background: RING,
};

export const Avatar: React.FC<AvatarProps> = ({
  avatarKey,
  size = 24,
  alt,
  testId,
  variant = "list",
  presence,
}) => {
  const { t } = useTranslation("common");
  const { data: blobUrl } = useAvatarBlobUrl(avatarKey ?? null);
  const resolvedAlt = alt ?? t("a11y.avatar");

  const dim = `${size}px`;
  const isProfile = variant === "profile";

  // A person is a circle; the profile page's large avatar is a panel-cornered tile.
  const shapeClass = isProfile ? "rounded-panel" : "rounded-full";
  const containerStyle: React.CSSProperties = {
    width: dim,
    height: dim,
    overflow: "hidden",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    background: "var(--c-surface, var(--c-bg))",
    border: isProfile ? "3px solid var(--c-accent)" : "none",
    flexShrink: 0,
  };

  const dotSize = Math.max(5, Math.round(size * 0.24));
  const wrapperStyle: React.CSSProperties = {
    position: "relative",
    display: "inline-flex",
    width: dim,
    height: dim,
    flexShrink: 0,
  };
  const dotStyle: React.CSSProperties = {
    position: "absolute",
    right: -2,
    bottom: -2,
    width: dotSize,
    height: dotSize,
    background: presence ? PRESENCE_COLORS[presence] : "transparent",
    border:
      presence === "offline"
        ? "2px solid var(--c-accent-muted)"
        : `2px solid ${RING}`,
    boxSizing: "content-box",
    overflow: "hidden",
  };

  const inner = blobUrl ? (
    <span style={containerStyle} className={shapeClass} data-testid={testId}>
      <img
        src={blobUrl}
        alt={resolvedAlt}
        style={{ width: "100%", height: "100%", objectFit: "cover" }}
      />
    </span>
  ) : (
    <span style={containerStyle} className={shapeClass} data-testid={testId} aria-label={resolvedAlt}>
      <User
        size={Math.round(size * 0.6)}
        aria-hidden="true"
        className="text-muted"
      />
    </span>
  );

  if (!presence) {
    return inner;
  }

  return (
    <span style={wrapperStyle}>
      {inner}
      <span
        data-testid={testId ? `${testId}-presence` : undefined}
        aria-label={t("presence.label", { status: t(`presence.${presence}`) })}
        data-presence={presence}
        className="rounded-full"
        style={dotStyle}
      >
        {presence === "away" ? <span aria-hidden="true" style={AWAY_BITE} /> : null}
      </span>
    </span>
  );
};
