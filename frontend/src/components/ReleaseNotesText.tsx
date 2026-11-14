import React, { useMemo } from "react";
import { parseReleaseNotes, type NoteSpan } from "../utils/releaseNotes";

interface ReleaseNotesTextProps {
  notes: string;
  "data-testid"?: string;
}

const Spans: React.FC<{ spans: NoteSpan[] }> = ({ spans }) => (
  <>
    {spans.map((span, i) => {
      if (span.style === "bold") {
        return (
          <strong key={i} className="font-medium text-fg">
            {span.text}
          </strong>
        );
      }
      if (span.style === "code") {
        return (
          <code key={i} className="font-mono text-fg break-words">
            {span.text}
          </code>
        );
      }
      return <React.Fragment key={i}>{span.text}</React.Fragment>;
    })}
  </>
);

/**
 * A release's changelog entry (#1916). The text comes from the CDN's updater feed, so every
 * character is a React text node: no HTML, no links. The notes are written in English whatever
 * the app's language, hence the fixed `lang` and direction.
 */
export const ReleaseNotesText: React.FC<ReleaseNotesTextProps> = ({ notes, "data-testid": testId }) => {
  const blocks = useMemo(() => parseReleaseNotes(notes), [notes]);
  return (
    <div
      lang="en"
      dir="ltr"
      data-testid={testId}
      className="flex flex-col gap-3 text-xs font-mono text-muted break-words"
    >
      {blocks.map((block, i) => {
        if (block.kind === "heading") {
          return (
            <h3 key={i} className="text-xs font-mono font-medium text-dim">
              <Spans spans={block.spans} />
            </h3>
          );
        }
        if (block.kind === "list") {
          return (
            <ul key={i} className="flex flex-col gap-2 list-disc ps-4">
              {block.items.map((item, j) => (
                <li key={j}>
                  <Spans spans={item} />
                </li>
              ))}
            </ul>
          );
        }
        return (
          <p key={i}>
            <Spans spans={block.spans} />
          </p>
        );
      })}
    </div>
  );
};
