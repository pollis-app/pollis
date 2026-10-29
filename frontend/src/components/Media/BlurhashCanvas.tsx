import React, { useEffect, useRef } from "react";
import { decode } from "blurhash";

/**
 * A blurhash drawn to fill whatever box it is put in. Every caller is a thumb
 * that crops the real image with `object-fit: cover`, so the placeholder
 * crops the same way. An invalid hash leaves the canvas blank rather than
 * throwing.
 */
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
      // Invalid or unsupported blurhash — canvas stays blank, no crash.
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
