import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { invoke, Channel } from "../bridge";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { CanvasAddon } from "@xterm/addon-canvas";
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
    // Renderer, best first. xterm has three and the gap between them is the
    // difference between typing feeling instant and feeling staggered:
    // WebGL > canvas > DOM, and DOM repaints spans on the CPU.
    //
    // This matters far more on Linux than macOS. WKWebView is accelerated, so
    // macOS gets WebGL. On Linux the app launches with
    // WEBKIT_DISABLE_COMPOSITING_MODE=1 (src-tauri/src/main.rs — it is there to
    // stop WebKitGTK aborting at startup on drivers without working GBM/EGL),
    // and with accelerated compositing off WebKitGTK has no WebGL at all. The
    // previous code caught that failure and said nothing, so every Linux user
    // silently fell all the way to DOM. Canvas is the rung in between and needs
    // no GPU compositing, which is why it is worth carrying.
    let renderer: "webgl" | "canvas" | "dom" = "dom";
    try {
      term.loadAddon(new WebglAddon());
      renderer = "webgl";
    } catch {
      try {
        term.loadAddon(new CanvasAddon());
        renderer = "canvas";
      } catch {
        // Both unavailable — xterm uses its DOM renderer.
      }
    }
    // Logged because it is otherwise invisible: the addons fail by throwing at
    // load time, so "is this accelerated?" was previously unanswerable without
    // a debugger. If this ever reads `dom` on a machine that should manage
    // better, that is the bug, not the terminal code.
    console.info(`[terminal] renderer: ${renderer}`);

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
    // Acks are COALESCED, not one per rendered chunk.
    //
    // The write path is binary, but this ack is JSON — and it used to fire from
    // every single xterm write callback. While you type, the shell echoes each
    // keystroke as its own chunk, so every character cost a JSON
    // serialize/parse round trip on top of the keystroke's own invoke: two IPC
    // hops per key, on WebKitGTK, which is the webview least able to afford
    // them. That is a large part of why typing felt staggered on Linux and fine
    // on macOS.
    //
    // Safe to batch because the aggregator needs EVENTUAL credit, not
    // per-chunk precision: it only parks above HIGH_WATERMARK (1 MiB) and
    // resumes below LOW_WATERMARK (256 KiB) — see commands/terminal_unix.rs.
    // So we flush on a frame, and immediately once enough is outstanding that
    // sitting on it could park a bulk producer mid-stream.
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

    // Best-effort PTY teardown on window close so no zombie shell is
    // left behind (Drop in Rust also covers process exit).
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
      // Drop any scheduled ack: the session is being closed, so the credit has
      // nowhere to go and the callback would fire against a dead id.
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
    // Deliberately empty: this effect owns the PTY for the app's lifetime.
    // `t` is captured on purpose — the only copy it produces is a one-shot
    // spawn failure written into the scrollback, and re-running on a language
    // change would kill and respawn the user's shell.
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

// Default export so AppShell can `lazy(() => import("../TerminalView"))` —
// this keeps the ~380 KiB xterm + WebGL addon out of the initial chunk and
// defers its parse/eval until the terminal is first opened (#431).
export default TerminalView;
