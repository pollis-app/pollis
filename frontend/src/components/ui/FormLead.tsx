import React from "react";

interface FormLeadProps {
  /** The step's question or heading; the page title stays `PageShell`'s. */
  title: React.ReactNode;
  /** One or two plain sentences under it. */
  children?: React.ReactNode;
  testId?: string;
  titleTestId?: string;
}

/** The lead of a form or a step of one (#1968): a heading and the sentence that explains it. */
export const FormLead: React.FC<FormLeadProps> = ({ title, children, testId, titleTestId }) => (
  <div className="flex flex-col gap-2" data-testid={testId}>
    <h2 data-testid={titleTestId} className="text-title text-fg">
      {title}
    </h2>
    {children && <p className="text-body leading-relaxed text-muted">{children}</p>}
  </div>
);
