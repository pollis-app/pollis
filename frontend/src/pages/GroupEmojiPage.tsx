import React from "react";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { GroupEmoji } from "./GroupEmoji";

export const GroupEmojiPage: React.FC = () => {
  const { groupId } = useParams({ from: "/groups/$groupId/emoji" });

  return (
    <PageShell title="Custom Emoji" scrollable>
      <GroupEmoji groupId={groupId} />
    </PageShell>
  );
};
