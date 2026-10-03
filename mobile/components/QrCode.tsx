import { useMemo } from "react";
import Svg, { Path, Rect } from "react-native-svg";
import QRCode from "qrcode";

// Defaults are scanner-safe black on white. Callers may theme it, but keep the
// polarity: DARK modules on a LIGHT field (e.g. the app background on the
// accent). An inverted, light-on-dark code fails on some camera decoders.
const DARK = "#000000";
const LIGHT = "#ffffff";
const QUIET = 2;

/**
 * A QR code drawn with react-native-svg from `qrcode`'s module matrix — the
 * encoder only, no canvas. Used for the QR device link (#1207).
 */
export function QrCode({
  value,
  size,
  testID,
  dark = DARK,
  light = LIGHT,
}: {
  value: string;
  size: number;
  testID?: string;
  dark?: string;
  light?: string;
}) {
  const { path, cells } = useMemo(() => {
    const qr = QRCode.create(value, { errorCorrectionLevel: "M" });
    const n = qr.modules.size;
    let d = "";
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        if (qr.modules.get(y, x)) {
          d += `M${x + QUIET} ${y + QUIET}h1v1h-1z`;
        }
      }
    }
    return { path: d, cells: n + QUIET * 2 };
  }, [value]);

  return (
    <Svg testID={testID} width={size} height={size} viewBox={`0 0 ${cells} ${cells}`}>
      <Rect x={0} y={0} width={cells} height={cells} fill={light} />
      <Path d={path} fill={dark} />
    </Svg>
  );
}
