import React from "react";
import { User } from "lucide-react";
import { useAvatarBlobUrl } from "../../hooks/queries/useUserProfile";

interface AvatarProps {
  avatarKey?: string | null;
  size?: number;
  alt?: string;
  testId?: string;
  variant?: "list" | "profile";
}

export const Avatar: React.FC<AvatarProps> = ({
  avatarKey,
  size = 24,
  alt = "Avatar",
  testId,
  variant = "list",
}) => {
  const { data: blobUrl } = useAvatarBlobUrl(avatarKey ?? null);

  const dim = `${size}px`;
  const isProfile = variant === "profile";

  const containerStyle: React.CSSProperties = {
    width: dim,
    height: dim,
    borderRadius: isProfile ? "0.5rem" : "50%",
    overflow: "hidden",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    background: "var(--c-surface, var(--c-bg))",
    border: isProfile ? "3px solid var(--c-accent)" : "none",
    flexShrink: 0,
  };

  if (blobUrl) {
    return (
      <span style={containerStyle} data-testid={testId}>
        <img
          src={blobUrl}
          alt={alt}
          style={{ width: "100%", height: "100%", objectFit: "cover" }}
        />
      </span>
    );
  }

  return (
    <span style={containerStyle} data-testid={testId} aria-label={alt}>
      <User
        size={Math.round(size * 0.6)}
        aria-hidden="true"
        style={{ color: "var(--c-text-muted)" }}
      />
    </span>
  );
};
