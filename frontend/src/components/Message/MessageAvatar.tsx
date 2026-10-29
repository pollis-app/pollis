import React from "react";
import { Avatar } from "../ui/Avatar";
import { useOtherUserProfile } from "../../hooks/queries/useUserProfile";

interface MessageAvatarProps {
  userId: string;
  username: string;
  size?: number;
}

// Refined-skin sender avatar. Outside the terminal path so its profile query
// only fires when an avatar is shown; React Query dedupes shared senders.
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
