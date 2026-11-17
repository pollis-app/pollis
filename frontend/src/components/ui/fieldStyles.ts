import type { CSSProperties } from "react";
import { FOCUS_RING } from "./focusRing.ts";

/** Class list of the `TextInput` / `TextArea` control: focus ring, surface, and a border that tracks error and focus. */
export function fieldControlClass(extra: string, error: string | undefined, isFocused: boolean): string {
  // A control's edge is `line-strong` (3:1, design/theme.ts); `line` is for decorative separators only.
  const border = error ? "border-danger" : isFocused ? "border-accent" : "border-line-strong";
  // `rounded-control`, the per-skin radius buttons use, so a field and a button on one form share a corner (#1964).
  return `w-full py-2 ${extra} font-mono text-body rounded-control ${FOCUS_RING} transition-all bg-surface text-fg border-2 ${border}`;
}

/** Runtime style of the same control: the focus chevron takes the start padding; disabled dims it. */
export function fieldControlStyle(isFocused: boolean, disabled: boolean): CSSProperties {
  return {
    paddingInlineStart: isFocused && !disabled ? "1.5rem" : "0.75rem",
    paddingInlineEnd: "0.75rem",
    outline: "none",
    opacity: disabled ? 0.5 : 1,
    cursor: disabled ? "not-allowed" : undefined,
  };
}
