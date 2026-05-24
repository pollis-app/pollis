import React, { useEffect, useRef } from "react";
import { invoke, Channel } from "../bridge";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";

interface TerminalViewProps {
  // True when the terminal pane is the active view. The component stays
  // mounted across toggles (so the PTY + scrollback survive); we just
  // refit/refocus when it becomes visible again.
  visible: boolean;
}

function cssVar(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return v.length > 0 ? v : fallback;
}

/**
 * Real terminal emulator pane. Spawns the user's $SHELL behind a PTY in
 * Rust on first mount and keeps it alive for the app's lifetime. Renders
 * with xterm.js + the WebGL addon.
 */
export const TerminalView: React.FC<TerminalViewProps> = ({ visible }) => {
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
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // WebGL unavailable (some Linux WebKitGTK builds) — xterm falls
      // back to its DOM renderer automatically.
    }

    termRef.current = term;
    fitRef.current = fit;

    // The renderer's dimensions aren't computed until the frame after
    // open(); fitting (or letting the ResizeObserver fit) before that
    // throws inside xterm. Gate everything on this.
    let ready = false;
    const safeFit = () => {
      if (!ready) {
        return;
      }
      try {
        fit.fit();
      } catch {
        /* container momentarily zero-sized (hidden) — ignore */
      }
    };
    // Binary IPC: bytes arrive as an ArrayBuffer (InvokeResponseBody::Raw)
    // with no JSON number-array bloat / parse. Hand the raw Uint8Array to
    // xterm — its write() has an internal UTF-8 decoder that correctly
    // holds partial multi-byte sequences split across chunks, so we must
    // NOT TextDecode per-chunk. The write callback fires once the chunk is
    // actually parsed/rendered: that's the true end-to-end backpressure
    // signal we credit back to the aggregator via terminal_ack.
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buf) => {
      const bytes = new Uint8Array(buf);
      term.write(bytes, () => {
        const id = terminalIdRef.current;
        if (id === null) {
          return;
        }
        invoke("terminal_ack", {
          terminalId: id,
          bytes: bytes.byteLength,
        }).catch(() => {});
      });
    };

    let disposed = false;

    // Spawn the PTY only after the first fit, so the shell inherits the
    // real COLUMNS/LINES. Opening it earlier with the unfitted xterm
    // default (80x24) makes zsh compute its PROMPT_SP eol-mark padding for
    // the wrong width — leaving a stray "%" line above every prompt that
    // the next prompt never overwrites.
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
            invoke("terminal_close", { terminalId: id }).catch(() => {});
            return;
          }
          terminalIdRef.current = id;
          term.focus();
        })
        .catch((err) => {
          term.write(`\r\n\x1b[31mfailed to start shell: ${err}\x1b[0m\r\n`);
        });
    });

    // Binary IPC input, symmetric with the output Channel above: hand the
    // raw UTF-8 bytes straight to invoke() as the request body (Tauri 2
    // accepts a Uint8Array as InvokeArgs and ships it as InvokeBody::Raw,
    // bypassing JSON entirely). The terminal id rides in a header so the
    // body stays a pure byte stream. The pre-binary path was
    // `data: Array.from(encoder.encode(data))` which expanded every
    // keystroke into a JSON number array — a per-key serialize/parse
    // roundtrip noticeable as input lag on WebKitGTK/X11.
    const encoder = new TextEncoder();
    const onDataDisposable = term.onData((data) => {
      const id = terminalIdRef.current;
      if (id === null) {
        return;
      }
      invoke("terminal_write", encoder.encode(data), {
        headers: { "x-terminal-id": id },
      }).catch(() => {});
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
        }).catch(() => {});
      }
    });
    resizeObserver.observe(containerRef.current);

    // Best-effort PTY teardown on window close so no zombie shell is
    // left behind (Drop in Rust also covers process exit).
    const onBeforeUnload = () => {
      const id = terminalIdRef.current;
      if (id !== null) {
        invoke("terminal_close", { terminalId: id }).catch(() => {});
      }
    };
    window.addEventListener("beforeunload", onBeforeUnload);

    return () => {
      disposed = true;
      cancelAnimationFrame(readyRaf);
      window.removeEventListener("beforeunload", onBeforeUnload);
      resizeObserver.disconnect();
      onDataDisposable.dispose();
      const id = terminalIdRef.current;
      if (id !== null) {
        invoke("terminal_close", { terminalId: id }).catch(() => {});
      }
      term.dispose();
    };
  }, []);

  // Becoming visible after a toggle: the container had zero size while
  // hidden, so refit and hand focus back to the shell.
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
        }).catch(() => {});
      }
      term.focus();
    });
    return () => cancelAnimationFrame(raf);
  }, [visible]);

  return (
    <div
      data-testid="terminal-view"
      ref={containerRef}
      style={{
        flex: 1,
        minWidth: 0,
        minHeight: 0,
        overflow: "hidden",
        background: "var(--c-bg)",
        padding: "6px 8px",
      }}
    />
  );
};
