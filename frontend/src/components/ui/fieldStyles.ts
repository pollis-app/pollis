import type { CSSProperties } from "react";

/** Class list of the `TextInput` / `TextArea` control: focus ring, surface, and a border that tracks error and focus. */
export function fieldControlClass(extra: string, error: string | undefined, isFocused: boolean): string {
  const border = error ? "border-danger" : isFocused ? "border-line-strong" : "border-line";
  return `w-full py-2 ${extra} font-mono text-sm focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black transition-all bg-surface text-fg border-2 ${border}`;
}

/** Runtime style of the same control: the focus chevron takes the start padding; disabled dims it. */
export function fieldControlStyle(isFocused: boolean, disabled: boolean): CSSProperties {
  return {
    paddingInlineStart: isFocused && !disabled ? "1.5rem" : "0.75rem",
    paddingInlineEnd: "0.75rem",
    outline: "none",
    borderRadius: "0.5rem",
    opacity: disabled ? 0.5 : 1,
    cursor: disabled ? "not-allowed" : undefined,
  };
}
