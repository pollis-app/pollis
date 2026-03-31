import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { CreateGroup } from "./CreateGroup";

export const CreateGroupPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Create Group" onBack={() => navigate({ to: "/groups" })}>
      <CreateGroup onSuccess={(groupId) => navigate({ to: "/groups/$groupId", params: { groupId } })} />
    </PageShell>
  );
};
