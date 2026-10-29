import { useMemo } from "react";
import { View } from "react-native";
import Svg, { Path, Rect } from "react-native-svg";
import QRCode from "qrcode";

// Keep the polarity, DARK on LIGHT: inverted codes fail some decoders.
const DARK = "#000000";
const LIGHT = "#ffffff";
const QUIET = 2;

/** A QR code drawn with react-native-svg from `qrcode`'s module matrix (#1207). */
export function QrCode({
  value,
  size,
  testID,
  dark = DARK,
  light = LIGHT,
  frame = 0,
  radius = 0,
}: {
  value: string;
  size: number;
  testID?: string;
  dark?: string;
  light?: string;
  // A light rounded tile around the code, keeping the polarity at the edge.
  frame?: number;
  radius?: number;
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

  const svg = (
    <Svg testID={testID} width={size} height={size} viewBox={`0 0 ${cells} ${cells}`}>
      <Rect x={0} y={0} width={cells} height={cells} fill={light} />
      <Path d={path} fill={dark} />
    </Svg>
  );
  if (frame <= 0 && radius <= 0) {
    return svg;
  }
  return (
    <View style={{ padding: frame, borderRadius: radius, backgroundColor: light }}>{svg}</View>
  );
}
