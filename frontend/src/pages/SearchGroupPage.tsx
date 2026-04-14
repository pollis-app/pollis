import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { SearchGroup } from "./SearchGroup";

export const SearchGroupPage: React.FC = () => {
  return (
    <PageShell title="Find Group">
      <SearchGroup />
    </PageShell>
  );
};
