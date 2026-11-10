// The app's own paste, for webviews whose native paste can hang (#1794).
//
// On X11, WebKitGTK's paste asks the clipboard owner for targets the owner never
// offered and waits ~30 s on each one it leaves unanswered. Steam's "Copy to
// clipboard" leaves them all unanswered, so the page froze before any paste
// handler ran. Where `app_handles_paste` says so, Ctrl+V never reaches WebKit:
// the text comes from a bounded Rust read, and a synthetic `paste` event carries
// it to the focused field, so the composer's file and image fallback still runs.

export interface PasteKey {
  key: string;
  code: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  preventDefault(): void;
}

/** Ctrl+V, Ctrl+Shift+V (paste as plain text) and Shift+Insert. */
export function isPasteChord(e: PasteKey): boolean {
  if (e.altKey || e.metaKey) {
    return false;
  }
  if (e.ctrlKey) {
    return e.code === "KeyV" || e.key.toLowerCase() === "v";
  }
  return e.shiftKey && e.key === "Insert";
}

export interface AppPasteDeps<T> {
  readText: () => Promise<string | null>;
  /** The focused editable element, or null. */
  focused: () => T | null;
  /** Whether `target` still has focus once the read lands. */
  isCurrent: (target: T) => boolean;
  /** Hand `text` ("" when the clipboard has none) to `target` as a paste. */
  deliver: (target: T, text: string) => void;
}

export type AppPasteResult = "pasted" | "busy" | "no-target" | "stale";

export function createAppPaste<T>(deps: AppPasteDeps<T>) {
  let inFlight = false;

  /** One read at a time: a repeat while one waits on a slow owner is dropped, not queued. */
  async function paste(): Promise<AppPasteResult> {
    if (inFlight) {
      return "busy";
    }
    const target = deps.focused();
    if (!target) {
      return "no-target";
    }
    inFlight = true;
    try {
      const text = await deps.readText().catch(() => null);
      if (!deps.isCurrent(target)) {
        return "stale";
      }
      // Delivered even without text: the composer reads files and images on any paste.
      deps.deliver(target, text ?? "");
      return "pasted";
    } finally {
      inFlight = false;
    }
  }

  function onKeyDown(e: PasteKey): void {
    if (!isPasteChord(e)) {
      return;
    }
    // Every paste chord, editable focus or not: WebKit must never start its own read.
    e.preventDefault();
    void paste();
  }

  return { paste, onKeyDown };
}

const TEXT_INPUT_TYPES = new Set(["text", "search", "email", "url", "tel", "password", "number"]);

function editable(el: Element | null): HTMLElement | null {
  if (el instanceof HTMLTextAreaElement) {
    return el.disabled || el.readOnly ? null : el;
  }
  if (el instanceof HTMLInputElement) {
    return TEXT_INPUT_TYPES.has(el.type) && !el.disabled && !el.readOnly ? el : null;
  }
  return el instanceof HTMLElement && el.isContentEditable ? el : null;
}

/** A `paste` event the field's handlers see as usual; unhandled text is inserted as typing is. */
function deliverPaste(target: HTMLElement, text: string): void {
  const data = new DataTransfer();
  if (text) {
    data.setData("text/plain", text);
  }
  const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true, composed: true });
  const unhandled = target.dispatchEvent(event);
  if (unhandled && text) {
    // insertText keeps the field's undo history and fires `input` like a keystroke.
    document.execCommand("insertText", false, text);
  }
}

/**
 * Take over paste for the whole window. Also exposed as `window.__pollisAppPaste`
 * for the native context menu's Paste item (`reroute_context_menu_paste`).
 */
export function installAppPaste(readText: () => Promise<string | null>): void {
  const app = createAppPaste<HTMLElement>({
    readText,
    focused: () => editable(document.activeElement),
    isCurrent: (target) => target.isConnected && document.activeElement === target,
    deliver: deliverPaste,
  });
  // Capture phase on window: ahead of every other keydown handler.
  window.addEventListener("keydown", (e) => app.onKeyDown(e), true);
  (window as unknown as { __pollisAppPaste: () => void }).__pollisAppPaste = () => {
    void app.paste();
  };
}
