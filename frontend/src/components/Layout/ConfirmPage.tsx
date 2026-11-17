import React from "react";
import { PageShell } from "./PageShell";
import { PageColumn } from "../ui/PageColumn";
import { ConfirmBlock } from "../ui/ConfirmBlock";

type ConfirmPageProps = React.ComponentProps<typeof ConfirmBlock> & {
  /** The page title (`PageShell`'s <h1>). */
  title: string;
  paneTestId?: string;
};

/** A confirmation that is its own page (#1968): `PageShell` + the page column + `ConfirmBlock`. */
export const ConfirmPage: React.FC<ConfirmPageProps> = ({ title, paneTestId, ...block }) => (
  <PageShell title={title}>
    <PageColumn paneTestId={paneTestId}>
      <ConfirmBlock {...block} />
    </PageColumn>
  </PageShell>
);
