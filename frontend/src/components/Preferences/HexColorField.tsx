import React from "react";
import { FOCUS_RING, FOCUS_WITHIN_RING } from "../ui/focusRing";

function isValidHex(val: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(val);
}

interface HexColorFieldProps {
  /** The colour in effect, as `#rrggbb`. */
  color: string;
  /** The hex text box's draft; may be invalid while typing. */
  hexInput: string;
  onHexInputChange: (value: string) => void;
  /** A valid colour was picked or typed. */
  onColor: (hex: string) => void;
  /** Tooltip of the swatch. */
  title: string;
  /** Ring the swatch in the accent, so a background-coloured swatch stays visible on the background. */
  outlined?: boolean;
}

/** A colour swatch opening the native picker, beside a hex text box that applies as soon as it is valid and reverts on blur if not. */
export const HexColorField: React.FC<HexColorFieldProps> = ({
  color,
  hexInput,
  onHexInputChange,
  onColor,
  title,
  outlined = false,
}) => (
  <div className="flex items-center gap-2">
    <label
      className={`flex-shrink-0 w-10 h-10 p-0 rounded-control cursor-pointer overflow-hidden ${
        outlined ? "outline outline-2 outline-accent -outline-offset-1" : ""
      } ${FOCUS_WITHIN_RING}`}
      title={title}
    >
      <input
        type="color"
        value={color}
        onChange={(e) => onColor(e.target.value)}
        className="border-0"
        style={{ width: "150%", height: "150%", margin: "-25%", padding: 0, cursor: "pointer" }}
      />
    </label>
    <input
      type="text"
      dir="ltr"
      value={hexInput}
      onChange={(e) => {
        const val = e.target.value;
        onHexInputChange(val);
        if (isValidHex(val)) {
          onColor(val);
        }
      }}
      onBlur={() => {
        if (!isValidHex(hexInput)) {
          onHexInputChange(color);
        }
      }}
      maxLength={7}
      spellCheck={false}
      className={`w-24 rounded-control text-secondary font-mono font-machine px-2 py-1 bg-surface border border-line ${isValidHex(hexInput) ? "text-fg" : "text-danger"} ${FOCUS_RING}`}
    />
  </div>
);
