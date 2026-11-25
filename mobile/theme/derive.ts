// Mobile's view of the shared colour source (design/theme.ts): the Modern recipe, under the
// names mobile components already use. The values are computed there, never here.

import { contrast, deriveTokens } from "../../design/theme.ts";

export { contrast };

export type Theme = {
  accent: string;
  bg: string;
  text: string;
  dim: string;
  muted: string;
  panel: string;
  raised: string;
  high: string;
  hair: string;
  hairSoft: string;
  edge: string;
  accentFaint: string;
  accentSoft: string;
  accentMid: string;
  accentLine: string;
  // The ring of an offline presence dot (#2059), desktop's `accent-muted`.
  accentMuted: string;
  // A disabled primary button's fill (theme/button.ts).
  accentDisabled: string;
  onAccent: string;
  // The one destructive red (design/theme.ts), shared with desktop.
  danger: string;
  onDanger: string;
  // Advisory, never an action (design/theme.ts): a key that failed its audit (#2114).
  warning: string;
  backdrop: string;
};

/** Accepts "#rrggbb" or an "r, g, b" triplet for either base. */
export function deriveTheme(accentRgb: string, bgRgb: string): Theme {
  const t = deriveTokens(accentRgb, bgRgb, "modern");
  return {
    accent: t.accent,
    bg: t.bg,
    text: t.text,
    dim: t.textDim,
    muted: t.textMuted,
    panel: t.surface,
    raised: t.surfaceRaised,
    high: t.surfaceHigh,
    hair: t.border,
    hairSoft: t.borderSoft,
    edge: t.borderStrong,
    accentFaint: t.accentFaint,
    accentSoft: t.accentSoft,
    accentMid: t.accentMid,
    accentLine: t.accentLine,
    accentMuted: t.accentMuted,
    accentDisabled: t.accentDisabled,
    onAccent: t.onAccent,
    danger: t.danger,
    onDanger: t.onDanger,
    warning: t.warning,
    backdrop: t.backdrop,
  };
}
