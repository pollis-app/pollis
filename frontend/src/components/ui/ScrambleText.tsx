import React, { useEffect, useRef, useState } from "react";

const CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";

function randomChar(): string {
  return CHARS[Math.floor(Math.random() * CHARS.length)];
}

interface ScrambleTextProps {
  // The real text to reveal. When undefined/null, shows a scrambling placeholder.
  text: string | null | undefined;
  // Width of the scramble placeholder in characters (default: 20)
  placeholderLength?: number;
  // Delay between each typed character in ms (default: 18)
  typeSpeed?: number;
  // How often the scramble noise refreshes in ms (default: 60)
  scrambleInterval?: number;
  className?: string;
}

/**
 * ScrambleText — shows random cycling characters while loading, then
 * types the real text in one character at a time when it becomes available.
 *
 * This is a "scramble-reveal" / "decode" animation common in terminal UIs.
 */
export const ScrambleText: React.FC<ScrambleTextProps> = ({
  text,
  placeholderLength = 20,
  typeSpeed = 25,
  scrambleInterval = 100,
  className,
}) => {
  // If text is already available on mount, show it immediately — no animation needed.
  const [displayed, setDisplayed] = useState(() => text ?? "");
  const [phase, setPhase] = useState<"scramble" | "typing" | "done">(() =>
    text != null ? "done" : "scramble"
  );
  const prevText = useRef<string | null | undefined>(text);
  const typeIndex = useRef(0);
  const frameRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clear any pending timers on unmount
  useEffect(() => {
    return () => {
      if (frameRef.current !== null) {
        clearTimeout(frameRef.current);
      }
    };
  }, []);

  useEffect(() => {
    // Text just became available — start typing phase from scratch
    if (text != null && prevText.current == null) {
      prevText.current = text;
      typeIndex.current = 0;
      setPhase("typing");
      return;
    }

    // Text changed while already available — restart typing
    if (text != null && text !== prevText.current) {
      prevText.current = text;
      typeIndex.current = 0;
      setPhase("typing");
      return;
    }

    // Text removed — go back to scramble
    if (text == null && prevText.current != null) {
      prevText.current = null;
      setPhase("scramble");
      return;
    }
  }, [text]);

  // Scramble loop — runs while phase === "scramble"
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

  // Typing loop — runs while phase === "typing"
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
      // Ensure at least 1 character is always shown to prevent height jitter
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

  // Done — just show the full text
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
