import React, { useEffect, useRef } from "react";
import { PageShell } from "../components/Layout/PageShell";
import { startGame, type GameHandle } from "../arcade/game";

export const ArcadePage: React.FC = () => {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }
    const handle: GameHandle = startGame(canvas);
    return () => handle.stop();
  }, []);

  return (
    <PageShell title="ARCADE">
      <div
        className="w-full h-full flex"
        style={{ background: "#000" }}
        data-testid="arcade-page"
      >
        <canvas
          ref={canvasRef}
          className="w-full h-full block"
          style={{ imageRendering: "pixelated", cursor: "crosshair" }}
        />
      </div>
    </PageShell>
  );
};
