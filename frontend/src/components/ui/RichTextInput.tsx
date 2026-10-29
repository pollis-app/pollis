import React, { useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef } from "react";
import { resolveEmojiUrl } from "../Emoji/useEmojiImage";
import { probeRender } from "../../utils/renderProbe";
import {
  EMOJI_TOKEN_ATTR,
  GHOST_ATTR,
  VALUE_ATTR,
  emojiTokenAfter,
  emojiTokenBefore,
  pastedPlainText,
  richRuns,
  serializeRichInput,
} from "./richInputModel";

/**
 * The composer's text field: a `contentEditable` that behaves like a
 * `<textarea>`, with custom emoji as atomic inline images. The value is one
 * plain string with integer offsets (`richInputModel`); the DOM is a projection
 * read back after every edit.
 *
 * Children are built imperatively: a `contentEditable` reconciled by React
 * loses the caret on every tree change, so nothing else may render into this
 * element (the ghost is a node in here too). The DOM is rebuilt only when
 * `value` differs from what was last serialized out of it, so ordinary typing
 * and IME composition are never disturbed.
 */

/** 1x1 transparent GIF, the emoji's src until the real URL resolves. */
const PENDING_SRC =
  "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

const TEXT_NODE = 3;
const ELEMENT_NODE = 1;

export interface RichTextInputHandle {
  focus: () => void;
  blur: () => void;
  setSelection: (caret: number) => void;
  /** Splice text in at the last known selection and take focus (the picker's
   *  path: a `contentEditable` loses its selection across the blur). */
  insertText: (text: string) => void;
  /** The selection right now, as model offsets. Key handlers must read this,
   *  not the async `onSelectionChange` state (ArrowUp hand-off). */
  selection: () => { start: number; end: number };
}

export interface RichTextInputProps {
  /** The message, in wire form; the single source of truth. */
  value: string;
  onChange: (value: string, caret: number) => void;
  onSelectionChange?: (caret: number, collapsed: boolean) => void;
  /** Runs before this component's key handling; preventDefault claims the key. */
  onKeyDown?: (e: React.KeyboardEvent) => void;
  /** Return true when the paste was consumed (e.g. a file). The event is always
   *  defaultPrevented first: the browser's own paste is HTML. */
  onPaste?: (e: React.ClipboardEvent) => boolean;
  onFocus?: () => void;
  onBlur?: () => void;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  className?: string;
  style?: React.CSSProperties;
  ariaLabel?: string;
  testId?: string;
  ghost?: string;
  ghostTestId?: string;
  /** The composer inverts it when focused. */
  ghostClassName?: string;
}

function isEmojiNode(node: Node): node is HTMLElement {
  return node.nodeType === ELEMENT_NODE && (node as HTMLElement).hasAttribute(EMOJI_TOKEN_ATTR);
}

function isGhostNode(node: Node): boolean {
  return node.nodeType === ELEMENT_NODE && (node as HTMLElement).hasAttribute(GHOST_ATTR);
}

/** One custom emoji node; the token lives in an attribute, so serialization ignores what it renders. */
function buildEmojiNode(token: string, shortcode: string, contentHash: string): HTMLElement {
  const span = document.createElement("span");
  span.setAttribute(EMOJI_TOKEN_ATTR, token);
  // The atom: selected whole, never holding a caret.
  span.setAttribute("contenteditable", "false");
  span.setAttribute("data-testid", "composer-emoji");
  span.setAttribute("data-shortcode", shortcode);
  span.className = "rich-input-emoji";

  const img = document.createElement("img");
  img.src = PENDING_SRC;
  img.alt = `:${shortcode}:`;
  img.title = `:${shortcode}:`;
  img.draggable = false;
  span.appendChild(img);

  resolveEmojiUrl(contentHash)
    .then((url) => {
      img.src = url;
    })
    .catch(() => {
      // Like `CustomEmojiImage`: show the shortcode; the token still sends.
      span.textContent = `:${shortcode}:`;
      span.classList.add("rich-input-emoji-fallback");
    });

  return span;
}

function project(host: HTMLElement, value: string): void {
  const fragment = document.createDocumentFragment();
  for (const run of richRuns(value)) {
    if (run.kind === "text") {
      fragment.appendChild(document.createTextNode(run.text));
      continue;
    }
    fragment.appendChild(buildEmojiNode(run.token, run.shortcode, run.contentHash));
  }
  // pre-wrap gives a trailing newline no line box; this filler (skipped by the
  // serializer) makes Shift+Enter on the last line visible.
  if (value === "" || value.endsWith("\n")) {
    fragment.appendChild(document.createElement("br"));
  }
  host.replaceChildren(fragment);
}

/** The model offset of a DOM position, by serializing everything before it, so it can't drift from the serializer. */
function offsetOf(host: HTMLElement, node: Node | null, nodeOffset: number): number {
  if (!node || !host.contains(node)) {
    return serializeRichInput(host).length;
  }
  const range = document.createRange();
  range.setStart(host, 0);
  try {
    range.setEnd(node, nodeOffset);
  } catch {
    return serializeRichInput(host).length;
  }
  return serializeRichInput(range.cloneContents()).length;
}

/** The DOM position of a model offset; inverse of `offsetOf`. */
function locate(host: HTMLElement, offset: number): { node: Node; offset: number } {
  let remaining = Math.max(0, offset);

  const descend = (parent: Node): { node: Node; offset: number } | null => {
    const children = Array.from(parent.childNodes);
    for (let index = 0; index < children.length; index += 1) {
      const child = children[index];
      if (isGhostNode(child)) {
        continue;
      }
      if (child.nodeType === TEXT_NODE) {
        const length = (child.textContent ?? "").length;
        if (remaining <= length) {
          return { node: child, offset: remaining };
        }
        remaining -= length;
        continue;
      }
      if (child.nodeType !== ELEMENT_NODE) {
        continue;
      }
      if (isEmojiNode(child)) {
        if (remaining <= 0) {
          return { node: parent, offset: index };
        }
        const token = child.getAttribute(EMOJI_TOKEN_ATTR) ?? "";
        remaining -= token.length;
        // An offset inside a token snaps after it (a floor; the model never makes one).
        if (remaining <= 0) {
          return { node: parent, offset: index + 1 };
        }
        continue;
      }
      if ((child as HTMLElement).tagName === "BR") {
        if (remaining <= 0) {
          return { node: parent, offset: index };
        }
        if (index < children.length - 1) {
          remaining -= 1;
        }
        continue;
      }
      const nested = descend(child);
      if (nested) {
        return nested;
      }
    }
    return null;
  };

  return descend(host) ?? { node: host, offset: host.childNodes.length };
}

const RichTextInputInner: React.ForwardRefRenderFunction<
  RichTextInputHandle,
  RichTextInputProps
> = (
  {
    value,
    onChange,
    onSelectionChange,
    onKeyDown,
    onPaste,
    onFocus,
    onBlur,
    placeholder = "",
    disabled = false,
    autoFocus = false,
    className = "",
    style,
    ariaLabel,
    testId,
    ghost = "",
    ghostTestId = "inline-ghost",
    ghostClassName = "",
  },
  ref,
) => {
  probeRender("RichTextInput");
  const hostRef = useRef<HTMLDivElement>(null);
  // What the DOM holds, as wire text; `null` so the mount always projects.
  const lastSerializedRef = useRef<string | null>(null);
  const pendingCaretRef = useRef<number | null>(null);
  // Last selection inside this element, for insertions from controls that took focus.
  const lastSelectionRef = useRef<{ start: number; end: number }>({ start: 0, end: 0 });

  // Read through refs so the native listeners register once.
  const valueRef = useRef(value);
  valueRef.current = value;
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const onSelectionChangeRef = useRef(onSelectionChange);
  onSelectionChangeRef.current = onSelectionChange;

  const applySelection = useCallback((caret: number) => {
    const host = hostRef.current;
    if (!host) {
      return;
    }
    const target = locate(host, caret);
    const selection = document.getSelection();
    if (!selection) {
      return;
    }
    const range = document.createRange();
    try {
      range.setStart(target.node, target.offset);
    } catch {
      return;
    }
    range.collapse(true);
    selection.removeAllRanges();
    selection.addRange(range);
    // Recorded here, not left to the async `selectionchange`: otherwise a second
    // pick from the open picker landed BEFORE the first.
    lastSelectionRef.current = { start: caret, end: caret };
  }, []);

  const replaceRange = useCallback((start: number, end: number, text: string) => {
    const current = valueRef.current;
    const next = current.slice(0, start) + text + current.slice(end);
    const caret = start + text.length;
    pendingCaretRef.current = caret;
    lastSelectionRef.current = { start: caret, end: caret };
    onChangeRef.current(next, caret);
  }, []);

  const liveOffsets = useCallback((): { start: number; end: number } | null => {
    const host = hostRef.current;
    const selection = document.getSelection();
    if (!host || !selection || selection.rangeCount === 0) {
      return null;
    }
    const range = selection.getRangeAt(0);
    if (!host.contains(range.startContainer) || !host.contains(range.endContainer)) {
      return null;
    }
    // A `Range` is always ordered. The collapsed case (every keystroke)
    // measures once.
    const start = offsetOf(host, range.startContainer, range.startOffset);
    const end = range.collapsed ? start : offsetOf(host, range.endContainer, range.endOffset);
    return { start, end };
  }, []);

  /** The live selection, or the last one in here, clamped. Never null. */
  const selectionOffsets = useCallback((): { start: number; end: number } => {
    const live = liveOffsets();
    if (live) {
      lastSelectionRef.current = live;
      return live;
    }
    const length = valueRef.current.length;
    const remembered = lastSelectionRef.current;
    return {
      start: Math.min(remembered.start, length),
      end: Math.min(remembered.end, length),
    };
  }, [liveOffsets]);

  useImperativeHandle(
    ref,
    () => ({
      focus: () => hostRef.current?.focus(),
      blur: () => hostRef.current?.blur(),
      setSelection: (caret: number) => {
        hostRef.current?.focus();
        applySelection(caret);
        lastSelectionRef.current = { start: caret, end: caret };
      },
      insertText: (text: string) => {
        // Read BEFORE focusing: focus can collapse the selection in some engines.
        const offsets = selectionOffsets();
        hostRef.current?.focus();
        replaceRange(offsets.start, offsets.end, text);
      },
      selection: selectionOffsets,
    }),
    [applySelection, replaceRange, selectionOffsets],
  );

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return;
    }
    if (lastSerializedRef.current !== value) {
      project(host, value);
      lastSerializedRef.current = value;
    }
    host.setAttribute(VALUE_ATTR, value);
    const pending = pendingCaretRef.current;
    if (pending !== null) {
      pendingCaretRef.current = null;
      applySelection(pending);
    }
  }, [value, applySelection]);

  // The ghost as a trailing non-editable node, declared AFTER the projection
  // effect so it re-appends after a rebuild. Appending never moves a placed
  // caret, and ghosts are only offered at end-of-value.
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return;
    }
    const existing = host.querySelector(`[${GHOST_ATTR}]`) as HTMLElement | null;
    if (!ghost) {
      existing?.remove();
      return;
    }
    const node = existing ?? document.createElement("span");
    node.setAttribute(GHOST_ATTR, "");
    node.setAttribute("contenteditable", "false");
    node.setAttribute("aria-hidden", "true");
    node.setAttribute("data-testid", ghostTestId);
    node.className = ghostClassName;
    if (node.textContent !== ghost) {
      node.textContent = ghost;
    }
    if (node.parentNode !== host || node.nextSibling !== null) {
      host.appendChild(node);
    }
  }, [ghost, ghostTestId, ghostClassName, value]);

  const handleInput = useCallback(() => {
    const host = hostRef.current;
    if (!host) {
      return;
    }
    const next = serializeRichInput(host);
    // Recorded BEFORE telling the parent, so the projection leaves the caret alone.
    lastSerializedRef.current = next;
    const selection = document.getSelection();
    const caret =
      selection && host.contains(selection.focusNode)
        ? offsetOf(host, selection.focusNode, selection.focusOffset)
        : next.length;
    lastSelectionRef.current = { start: caret, end: caret };
    onChangeRef.current(next, caret);
  }, []);

  // Emoji deletion is decided against the model: engines differ on Backspace
  // next to a `contenteditable=false` element.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return;
    }
    const handler = (event: Event) => {
      const input = event as InputEvent;
      if (
        input.inputType !== "deleteContentBackward" &&
        input.inputType !== "deleteContentForward"
      ) {
        return;
      }
      const offsets = selectionOffsets();
      if (offsets.start !== offsets.end) {
        return;
      }
      const target =
        input.inputType === "deleteContentBackward"
          ? emojiTokenBefore(valueRef.current, offsets.start)
          : emojiTokenAfter(valueRef.current, offsets.start);
      if (!target) {
        return;
      }
      event.preventDefault();
      replaceRange(target.start, target.end, "");
    };
    host.addEventListener("beforeinput", handler);
    return () => host.removeEventListener("beforeinput", handler);
  }, [replaceRange, selectionOffsets]);

  // `selectionchange` fires on the document, so the in-here guard is load-bearing.
  useEffect(() => {
    const handler = () => {
      const host = hostRef.current;
      const selection = document.getSelection();
      if (!host || !selection || selection.rangeCount === 0) {
        return;
      }
      if (!host.contains(selection.focusNode)) {
        return;
      }
      const live = liveOffsets();
      if (!live) {
        return;
      }
      lastSelectionRef.current = live;
      const caret = selection.isCollapsed
        ? live.start
        : offsetOf(host, selection.focusNode, selection.focusOffset);
      onSelectionChangeRef.current?.(caret, selection.isCollapsed);
    };
    document.addEventListener("selectionchange", handler);
    return () => document.removeEventListener("selectionchange", handler);
  }, [liveOffsets]);

  useEffect(() => {
    if (autoFocus) {
      hostRef.current?.focus();
    }
  }, [autoFocus]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      onKeyDown?.(e);
      if (e.defaultPrevented) {
        return;
      }
      // Shift+Enter as one model character; engines would insert `<div>` or `<br>`.
      if (e.key === "Enter") {
        const offsets = selectionOffsets();
        e.preventDefault();
        replaceRange(offsets.start, offsets.end, "\n");
      }
    },
    [onKeyDown, replaceRange, selectionOffsets],
  );

  const handlePaste = useCallback(
    (e: React.ClipboardEvent) => {
      // Always: the browser's own paste inserts HTML.
      e.preventDefault();
      if (onPaste?.(e)) {
        return;
      }
      const text = pastedPlainText(e.clipboardData);
      if (!text) {
        return;
      }
      const offsets = selectionOffsets();
      replaceRange(offsets.start, offsets.end, text);
    },
    [onPaste, replaceRange, selectionOffsets],
  );

  // Copy and cut serialize the selection with the same walker, so tokens
  // survive the clipboard losslessly.
  const writeSelection = useCallback((e: React.ClipboardEvent): boolean => {
    const host = hostRef.current;
    const selection = document.getSelection();
    if (!host || !selection || selection.rangeCount === 0 || selection.isCollapsed) {
      return false;
    }
    const range = selection.getRangeAt(0);
    if (!host.contains(range.startContainer) || !host.contains(range.endContainer)) {
      return false;
    }
    e.clipboardData.setData("text/plain", serializeRichInput(range.cloneContents()));
    e.preventDefault();
    return true;
  }, []);

  const handleCopy = useCallback(
    (e: React.ClipboardEvent) => {
      writeSelection(e);
    },
    [writeSelection],
  );

  const handleCut = useCallback(
    (e: React.ClipboardEvent) => {
      const offsets = selectionOffsets();
      if (!writeSelection(e)) {
        return;
      }
      replaceRange(offsets.start, offsets.end, "");
    },
    [writeSelection, selectionOffsets, replaceRange],
  );

  return (
    <div
      ref={hostRef}
      data-testid={testId}
      contentEditable={!disabled}
      suppressContentEditableWarning
      role="textbox"
      aria-multiline="true"
      aria-label={ariaLabel}
      data-placeholder={placeholder}
      data-empty={value === "" ? "true" : "false"}
      spellCheck={false}
      autoCorrect="off"
      autoCapitalize="off"
      onInput={handleInput}
      onKeyDown={handleKeyDown}
      onPaste={handlePaste}
      onCopy={handleCopy}
      onCut={handleCut}
      onFocus={onFocus}
      onBlur={onBlur}
      className={className}
      style={style}
    />
  );
};

export const RichTextInput = React.forwardRef(RichTextInputInner);
