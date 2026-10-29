import React from "react";
import { Avatar } from "../ui/Avatar";
import { useOtherUserProfile } from "../../hooks/queries/useUserProfile";

interface MessageAvatarProps {
  userId: string;
  username: string;
  size?: number;
}

// Refined-skin avatar; kept off the terminal path so its profile query only fires when shown.
export const MessageAvatar: React.FC<MessageAvatarProps> = ({
  userId,
  username,
  size = 36,
}) => {
  const { data: profile } = useOtherUserProfile(userId);
  return (
    <Avatar
      avatarKey={profile?.avatar_url ?? null}
      size={size}
      alt={username}
    />
  );
};
