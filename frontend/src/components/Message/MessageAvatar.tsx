import React from "react";
import { Avatar } from "../ui/Avatar";
import { useOtherUserProfile } from "../../hooks/queries/useUserProfile";

interface MessageAvatarProps {
  userId: string;
  username: string;
  /** The author's alias picture in this group (#2003); the main profile's when absent. */
  aliasKey?: string | null;
  size?: number;
}

// Modern-skin avatar; kept off the terminal path so its profile query only fires when shown.
export const MessageAvatar: React.FC<MessageAvatarProps> = ({
  userId,
  username,
  aliasKey,
  size = 36,
}) => {
  const { data: profile } = useOtherUserProfile(userId);
  return (
    <Avatar
      avatarKey={aliasKey || (profile?.avatar_url ?? null)}
      size={size}
      alt={username}
    />
  );
};
