import React from "react";

interface ConversationHeaderProps {
  title: React.ReactNode;
  /** Header actions (icon buttons, pills). Spaced and centred here, never per item (#1640). */
  children?: React.ReactNode;
}

/** The channel / DM top bar: title on the start side, one evenly-gapped action row on the end side. */
export const ConversationHeader: React.FC<ConversationHeaderProps> = ({ title, children }) => (
  <div
    data-testid="conversation-header"
    className="flex items-center gap-4 px-4 flex-shrink-0 text-xs font-mono h-bar border-b border-line text-muted"
  >
    <div className="flex-1 min-w-0 truncate">{title}</div>
    {children && (
      <div
        data-testid="conversation-header-actions"
        className="flex items-center gap-2 flex-shrink-0"
      >
        {children}
      </div>
    )}
  </div>
);
