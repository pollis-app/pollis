import React from "react";
import { QRCodeSVG } from "qrcode.react";

interface ThemedQrProps {
  value: string;
  size: number;
  title?: string;
  "data-testid"?: string;
}

/**
 * A QR on a rounded accent tile, modules in the background colour (#1607).
 * Dark modules on the light accent keep normal polarity, which every camera decoder reads.
 */
export const ThemedQr: React.FC<ThemedQrProps> = ({ value, size, title, "data-testid": testId }) => (
  <div className="p-3 rounded-lg bg-accent">
    <QRCodeSVG
      data-testid={testId}
      value={value}
      size={size}
      bgColor="var(--c-accent)"
      fgColor="var(--c-bg)"
      marginSize={2}
      level="M"
      title={title}
    />
  </div>
);
