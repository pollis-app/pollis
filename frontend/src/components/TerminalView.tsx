import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { invoke, Channel } from "../bridge";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { CanvasAddon } from "@xterm/addon-canvas";
import "@xterm/xterm/css/xterm.css";

interface TerminalViewProps {
  // Stays mounted across toggles (PTY + scrollback survive).
  visible: boolean;
}

function cssVar(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return v.length > 0 ? v : fallback;
}

/** A $SHELL behind a PTY in Rust, kept for the app's lifetime; xterm.js renderer. */
const TerminalView: React.FC<TerminalViewProps> = ({ visible }) => {
  const { t } = useTranslation("nav");
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const terminalIdRef = useRef<string | null>(null);

  useEffect(() => {
    if (!containerRef.current) {
      return;
    }

    const term = new Terminal({
      fontFamily: cssVar("--font-mono", "ui-monospace, monospace"),
      fontSize: 13,
      cursorBlink: true,
      allowProposedApi: true,
      theme: {
        background: cssVar("--c-bg", "#000000"),
        foreground: cssVar("--c-text", "#cccccc"),
        cursor: cssVar("--c-accent", "#00ff00"),
        cursorAccent: cssVar("--c-bg", "#000000"),
        selectionBackground: cssVar("--c-accent-muted", "#264f78"),
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(containerRef.current);
    // Best renderer: WebGL > canvas > DOM. Linux runs with compositing off (no WebGL), so canvas matters there.
    let renderer: "webgl" | "canvas" | "dom" = "dom";
    try {
      term.loadAddon(new WebglAddon());
      renderer = "webgl";
    } catch {
      try {
        term.loadAddon(new CanvasAddon());
        renderer = "canvas";
      } catch {
        // Neither: DOM renderer.
      }
    }
    // Logged: otherwise invisible whether it is accelerated.
    console.info(`[terminal] renderer: ${renderer}`);

    termRef.current = term;
    fitRef.current = fit;

    // Fitting before the first frame after open() throws in xterm.
    let ready = false;
    const safeFit = () => {
      if (!ready) {
        return;
      }
      try {
        fit.fit();
      } catch {
        /* hidden (zero-sized); ignore */
      }
    };
    // Raw bytes to xterm (its decoder handles split UTF-8). Acks are coalesced per frame: the aggregator needs eventual credit only (watermarks in commands/terminal_unix.rs).
    let pendingAck = 0;
    let ackScheduled = false;
    let ackFrame = 0;
    const ACK_FLUSH_BYTES = 64 * 1024;
    const flushAck = () => {
      ackScheduled = false;
      const bytes = pendingAck;
      pendingAck = 0;
      const id = terminalIdRef.current;
      if (id === null || bytes === 0) {
        return;
      }
      invoke("terminal_ack", { terminalId: id, bytes }).catch((e) =>
        console.warn("terminal_ack failed", e),
      );
    };

    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buf) => {
      const bytes = new Uint8Array(buf);
      term.write(bytes, () => {
        pendingAck += bytes.byteLength;
        if (pendingAck >= ACK_FLUSH_BYTES) {
          flushAck();
          return;
        }
        if (!ackScheduled) {
          ackScheduled = true;
          ackFrame = requestAnimationFrame(flushAck);
        }
      });
    };

    let disposed = false;

    // Spawn after the first fit so zsh gets the real width (else a stray "%" line).
    const readyRaf = requestAnimationFrame(() => {
      ready = true;
      safeFit();
      invoke<string>("terminal_open", {
        rows: term.rows,
        cols: term.cols,
        onOutput: channel,
      })
        .then((id) => {
          if (disposed) {
            invoke("terminal_close", { terminalId: id }).catch((e) => console.warn("terminal_close failed", e));
            return;
          }
          terminalIdRef.current = id;
          term.focus();
        })
        .catch((err) => {
          term.write(
            `\r\n\x1b[31m${t("terminal.shellFailed", { error: err })}\x1b[0m\r\n`,
          );
        });
    });

    // Raw UTF-8 input as the invoke body; the terminal id rides in a header.
    const encoder = new TextEncoder();
    const onDataDisposable = term.onData((data) => {
      const id = terminalIdRef.current;
      if (id === null) {
        return;
      }
      invoke("terminal_write", encoder.encode(data), {
        headers: { "x-terminal-id": id },
      }).catch((e) => console.warn("terminal_write failed", e));
    });

    const resizeObserver = new ResizeObserver(() => {
      if (!ready) {
        return;
      }
      const id = terminalIdRef.current;
      safeFit();
      if (id !== null) {
        invoke("terminal_resize", {
          terminalId: id,
          rows: term.rows,
          cols: term.cols,
        }).catch((e) => console.warn("terminal_resize failed", e));
      }
    });
    resizeObserver.observe(containerRef.current);

    // Best-effort teardown on window close.
    const onBeforeUnload = () => {
      const id = terminalIdRef.current;
      if (id !== null) {
        invoke("terminal_close", { terminalId: id }).catch((e) => console.warn("terminal_close failed", e));
      }
    };
    window.addEventListener("beforeunload", onBeforeUnload);

    return () => {
      disposed = true;
      cancelAnimationFrame(readyRaf);
      window.removeEventListener("beforeunload", onBeforeUnload);
      resizeObserver.disconnect();
      // Drop any scheduled ack for the dead session.
      if (ackScheduled) {
        cancelAnimationFrame(ackFrame);
        ackScheduled = false;
      }
      onDataDisposable.dispose();
      const id = terminalIdRef.current;
      if (id !== null) {
        invoke("terminal_close", { terminalId: id }).catch((e) => console.warn("terminal_close failed", e));
      }
      term.dispose();
    };
    // Empty on purpose: owns the PTY for the app's lifetime; re-running would respawn the shell.
  }, []);

  // Refit and refocus on becoming visible.
  useEffect(() => {
    if (!visible) {
      return;
    }
    const term = termRef.current;
    const fit = fitRef.current;
    if (term === null || fit === null) {
      return;
    }
    const raf = requestAnimationFrame(() => {
      try {
        fit.fit();
      } catch {
        return;
      }
      const id = terminalIdRef.current;
      if (id !== null) {
        invoke("terminal_resize", {
          terminalId: id,
          rows: term.rows,
          cols: term.cols,
        }).catch((e) => console.warn("terminal_resize failed", e));
      }
      term.focus();
    });
    return () => cancelAnimationFrame(raf);
  }, [visible]);

  return (
    <div
      data-testid="terminal-view"
      ref={containerRef}
      className="bg-bg"
      style={{
        flex: 1,
        minWidth: 0,
        minHeight: 0,
        overflow: "hidden",
        padding: "6px 8px",
      }}
    />
  );
};

// Default export for AppShell's lazy import (#431).
export default TerminalView;
