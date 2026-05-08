import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { RenameGroup } from "./RenameGroup";

export const RenameGroupPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/rename" });

  return (
    <PageShell title="Rename Group">
      <RenameGroup
        groupId={groupId}
        onSuccess={() => {
          navigate({ to: "/groups/$groupId", params: { groupId } });
        }}
      />
    </PageShell>
  );
};
