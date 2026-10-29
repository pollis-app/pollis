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

/** Avatar with the live presence dot; silent when `userId` is null. */
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
