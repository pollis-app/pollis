import React, { useEffect, useRef, useState } from "react";

const CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";

function randomChar(): string {
  return CHARS[Math.floor(Math.random() * CHARS.length)];
}

interface ScrambleTextProps {
  // The text to reveal; null/undefined shows the scrambling placeholder.
  text: string | null | undefined;
  // Placeholder width in characters (default 20)
  placeholderLength?: number;
  // ms per typed character (default 18)
  typeSpeed?: number;
  // ms between scramble refreshes (default 60)
  scrambleInterval?: number;
  className?: string;
}

/** Cycles random characters while loading, then types the real text in. */
export const ScrambleText: React.FC<ScrambleTextProps> = ({
  text,
  placeholderLength = 20,
  typeSpeed = 25,
  scrambleInterval = 100,
  className,
}) => {
  // Text available on mount shows immediately, without animation.
  const [displayed, setDisplayed] = useState(() => text ?? "");
  const [phase, setPhase] = useState<"scramble" | "typing" | "done">(() =>
    text != null ? "done" : "scramble"
  );
  const prevText = useRef<string | null | undefined>(text);
  const typeIndex = useRef(0);
  const frameRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (frameRef.current !== null) {
        clearTimeout(frameRef.current);
      }
    };
  }, []);

  useEffect(() => {
    if (text != null && prevText.current == null) {
      prevText.current = text;
      typeIndex.current = 0;
      setPhase("typing");
      return;
    }

    if (text != null && text !== prevText.current) {
      prevText.current = text;
      typeIndex.current = 0;
      setPhase("typing");
      return;
    }

    if (text == null && prevText.current != null) {
      prevText.current = null;
      setPhase("scramble");
      return;
    }
  }, [text]);

  useEffect(() => {
    if (phase !== "scramble") {
      return;
    }
    const tick = () => {
      setDisplayed(
        Array.from({ length: placeholderLength }, randomChar).join("")
      );
      frameRef.current = setTimeout(tick, scrambleInterval);
    };
    tick();
    return () => {
      if (frameRef.current !== null) {
        clearTimeout(frameRef.current);
      }
    };
  }, [phase, placeholderLength, scrambleInterval]);

  useEffect(() => {
    if (phase !== "typing" || text == null) {
      return;
    }
    const tick = () => {
      const i = typeIndex.current;
      if (i > text.length) {
        setPhase("done");
        return;
      }
      // At least 1 character, to prevent height jitter.
      setDisplayed(text.slice(0, Math.max(1, i)));
      typeIndex.current = i + 1;
      frameRef.current = setTimeout(tick, typeSpeed);
    };
    tick();
    return () => {
      if (frameRef.current !== null) {
        clearTimeout(frameRef.current);
      }
    };
  }, [phase, text, typeSpeed]);

  useEffect(() => {
    if (phase === "done" && text != null) {
      setDisplayed(text);
    }
  }, [phase, text]);

  return (
    <span
      className={className}
      style={{ fontVariantNumeric: "tabular-nums" }}
      aria-label={text ?? undefined}
      aria-busy={phase !== "done"}
    >
      {displayed}
    </span>
  );
};
