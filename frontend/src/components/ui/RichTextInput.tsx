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

/** Composer field: a `contentEditable` projecting one plain wire string (see `richInputModel`). Children are built imperatively, since React reconciling them loses the caret; re-projected only when `value` differs from the last serialized DOM. */

/** A 1x1 transparent GIF: the emoji image's src until the real URL resolves. */
const PENDING_SRC =
  "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

const TEXT_NODE = 3;
const ELEMENT_NODE = 1;

export interface RichTextInputHandle {
  focus: () => void;
  blur: () => void;
  /** Put the collapsed caret at a model offset. */
  setSelection: (caret: number) => void;
  /** Splice text in at the last known selection (a contentEditable forgets it on blur). */
  insertText: (text: string) => void;
  /** Live selection as model offsets; key handlers read this, as `selectionchange` is async. */
  selection: () => { start: number; end: number };
}

export interface RichTextInputProps {
  /** Wire form; the single source of truth. */
  value: string;
  onChange: (value: string, caret: number) => void;
  onSelectionChange?: (caret: number, collapsed: boolean) => void;
  /** Runs first; preventDefault claims the key. */
  onKeyDown?: (e: React.KeyboardEvent) => void;
  /** Return true when consumed (e.g. a file). Always runs defaultPrevented: the browser's paste is HTML. */
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
  ghostClassName?: string;
}

function isEmojiNode(node: Node): node is HTMLElement {
  return node.nodeType === ELEMENT_NODE && (node as HTMLElement).hasAttribute(EMOJI_TOKEN_ATTR);
}

function isGhostNode(node: Node): boolean {
  return node.nodeType === ELEMENT_NODE && (node as HTMLElement).hasAttribute(GHOST_ATTR);
}

/** The token lives in an attribute, so serialization ignores what the node renders. */
function buildEmojiNode(token: string, shortcode: string, contentHash: string): HTMLElement {
  const span = document.createElement("span");
  span.setAttribute(EMOJI_TOKEN_ATTR, token);
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
  // pre-wrap gives a trailing newline no line box; this filler (skipped by the serializer) does.
  if (value === "" || value.endsWith("\n")) {
    fragment.appendChild(document.createElement("br"));
  }
  host.replaceChildren(fragment);
}

/** Model offset of a DOM position, by serializing the prefix, so it cannot drift from the serializer. */
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
        // Inside a token snaps to after it (never produced by the model).
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
  // null before the first projection, so the mount always projects.
  const lastSerializedRef = useRef<string | null>(null);
  const pendingCaretRef = useRef<number | null>(null);
  // Last selection inside this element, for inserts from controls that steal focus.
  const lastSelectionRef = useRef<{ start: number; end: number }>({ start: 0, end: 0 });

  // Read through refs so native listeners register once.
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
    // Recorded here: the async `selectionchange` may land after focus leaves, misplacing a second pick.
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
    // Collapsed case measures once.
    const start = offsetOf(host, range.startContainer, range.startOffset);
    const end = range.collapsed ? start : offsetOf(host, range.endContainer, range.endOffset);
    return { start, end };
  }, []);

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
        // Read offsets before focusing: focusing can collapse the selection.
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

  // Ghost as a trailing non-editable node; declared after projection so it re-appends.
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
    // Before telling the parent, so the projection effect leaves the caret alone.
    lastSerializedRef.current = next;
    const selection = document.getSelection();
    const caret =
      selection && host.contains(selection.focusNode)
        ? offsetOf(host, selection.focusNode, selection.focusOffset)
        : next.length;
    lastSelectionRef.current = { start: caret, end: caret };
    onChangeRef.current(next, caret);
  }, []);

  // Backspace by an emoji is decided against the model: engines differ next to contenteditable=false.
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
      // Shift+Enter: one model character, not an engine-specific `<div>`/`<br>`.
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

  // Copy/cut through the same walker, so tokens survive the clipboard.
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
