import React from "react";
import { LoadingSpinner } from "./LoaderSpinner";

/** A busy glyph that occupies exactly the box of the `size`px icon it replaces, so the button keeps its shape. */
export const IconSpinner: React.FC<{ size: number }> = ({ size }) => (
  <span
    className="inline-flex items-center justify-center overflow-hidden leading-none"
    style={{ width: size, height: size }}
  >
    <LoadingSpinner size="sm" tone="inherit" />
  </span>
);
