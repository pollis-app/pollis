import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { SearchGroup } from "./SearchGroup";

export const SearchGroupPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Find Group" onBack={() => navigate({ to: "/groups" })}>
      <SearchGroup />
    </PageShell>
  );
};
