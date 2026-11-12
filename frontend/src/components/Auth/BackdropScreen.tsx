import React from "react";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";

interface BackdropScreenProps {
  "data-testid": string;
  /** Opacity of the dot-matrix layer behind the content. */
  backdropOpacity: number;
  /** Dot-matrix animation speed; omitted keeps `DotMatrix`'s default. */
  backdropSpeed?: number;
  children: React.ReactNode;
}

/** Full-window pre-app screen: a dimmed `DotMatrix` behind the title bar and the screen's own content. */
export const BackdropScreen: React.FC<BackdropScreenProps> = ({
  "data-testid": testId,
  backdropOpacity,
  backdropSpeed,
  children,
}) => (
  <div
    data-testid={testId}
    className="flex flex-col h-full w-full bg-bg"
    style={{ position: "relative" }}
  >
    <div style={{ position: "absolute", inset: 0, opacity: backdropOpacity, pointerEvents: "none" }}>
      <DotMatrix speed={backdropSpeed} />
    </div>
    <TitleBar />
    {children}
  </div>
);
