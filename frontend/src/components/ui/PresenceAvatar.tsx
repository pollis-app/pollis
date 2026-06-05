import React from "react";
import { observer } from "mobx-react-lite";
import { Avatar } from "./Avatar";
import { usePresenceStatus } from "../../stores/presenceStore";

interface PresenceAvatarProps {
  userId: string | null | undefined;
  avatarKey?: string | null;
  size?: number;
  alt?: string;
  testId?: string;
  variant?: "list" | "profile";
}

/**
 * Avatar that overlays the live online/offline dot for a known user. Use
 * this in DM-call surfaces (DM list, search panel, profile page) and let
 * the dot sit silent when `userId` is null — the underlying Avatar handles
 * the no-presence case.
 */
export const PresenceAvatar: React.FC<PresenceAvatarProps> = observer(({
  userId,
  avatarKey,
  size,
  alt,
  testId,
  variant,
}) => {
  const status = usePresenceStatus(userId ?? null);
  return (
    <Avatar
      avatarKey={avatarKey}
      size={size}
      alt={alt}
      testId={testId}
      variant={variant}
      presence={userId ? status : undefined}
    />
  );
});
