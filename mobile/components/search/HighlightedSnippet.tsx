import { Text } from "react-native";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import type { SearchSnippet } from "../../hooks/queries/useSearch";
import { splitSnippet } from "../../lib/searchSnippet";

/**
 * `highlights` are `[start, end)` UTF-16 indices, so the text is sliced
 * directly and never rendered as markup (the snippet is user content).
 */
export function HighlightedSnippet({
  snippet,
  numberOfLines = 2,
}: {
  snippet: SearchSnippet;
  numberOfLines?: number;
}) {
  const parts = splitSnippet(snippet.text, snippet.highlights);

  return (
    <Text numberOfLines={numberOfLines} style={ty.secondary}>
      {parts.map((p, i) => (
        <Text
          key={i}
          style={
            p.hit
              ? {
                  color: semantic.accent,
                  fontFamily: fonts.semibold,
                  backgroundColor: semantic.accentSoft,
                }
              : undefined
          }
        >
          {p.text}
        </Text>
      ))}
    </Text>
  );
}
