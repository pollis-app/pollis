import React, { useEffect, useRef } from "react";
import { decode } from "blurhash";

/** A blurhash filling its box, cropped like `object-fit: cover`. */
export const BlurhashCanvas: React.FC<{ hash: string }> = ({ hash }) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }
    try {
      const pixels = decode(hash, 32, 32);
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        return;
      }
      const imageData = ctx.createImageData(32, 32);
      imageData.data.set(pixels);
      ctx.putImageData(imageData, 0, 0);
    } catch {
      // Invalid blurhash: canvas stays blank.
    }
  }, [hash]);

  return (
    <div style={{ position: "relative", width: "100%", height: "100%", overflow: "hidden" }}>
      <canvas
        ref={canvasRef}
        width={32}
        height={32}
        style={{
          position: "absolute",
          inset: 0,
          width: "100%",
          height: "100%",
          filter: "blur(6px)",
          transform: "scale(1.08)",
        }}
      />
    </div>
  );
};
