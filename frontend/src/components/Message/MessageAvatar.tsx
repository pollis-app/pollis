import React from "react";
import { Avatar } from "../ui/Avatar";
import { useOtherUserProfile } from "../../hooks/queries/useUserProfile";

interface MessageAvatarProps {
  userId: string;
  username: string;
  size?: number;
}

// Resolves a message sender's avatar key from their profile and renders the
// rounded avatar used by the refined skin's group-start rows. Kept out of the
// terminal render path so its profile query only fires when the refined skin
// actually shows an avatar. The underlying query is cached + deduped by
// React Query, so many rows sharing a sender cost a single fetch.
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
