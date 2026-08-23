import React, { useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef } from "react";
import { resolveEmojiUrl } from "../Emoji/useEmojiImage";
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
 * The composer's text field: a `contentEditable` that behaves like the
 * `<textarea>` it replaced, except that a custom emoji is an atomic inline
 * image rather than seventy characters of wire token.
 *
 * ## Why this shape
 *
 * The value stays ONE PLAIN STRING with integer offsets — see `richInputModel`.
 * Every consumer above it (mentions, `:shortcode:` completion, drafts, the send
 * path) still works on `(text, caret)` exactly as it did against a textarea, so
 * none of them had to learn a document model. The DOM here is a projection of
 * that string, read back with `serializeRichInput` after every edit.
 *
 * The children are built imperatively, not by React. That is not a shortcut: a
 * `contentEditable` whose children React reconciles loses the caret on every
 * keystroke that changes the tree, and there is no way to ask React to leave
 * the selection alone. So this component renders one empty `<div>` and owns
 * everything inside it. The rule that follows is that nothing else may render
 * children into this element — including the inline autosuggest, which is why
 * the old `InlineGhost` mirror is gone and the ghost is a node in here instead.
 *
 * ## Re-projection
 *
 * The DOM is rebuilt only when `value` differs from what was last serialized
 * out of it. Ordinary typing therefore never rebuilds anything (the value came
 * from the DOM, so the two agree), which is what keeps the caret and IME
 * composition intact. An external change — a picker insertion, a `:joy:`
 * substitution, a draft switch, a send that clears the box — does not agree,
 * and is rebuilt with the caret restored from a model offset.
 */

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
}

export interface RichTextInputProps {
  /** The message, in wire form. The single source of truth. */
  value: string;
  /** An edit happened: the new wire text, and where the caret ended up. */
  onChange: (value: string, caret: number) => void;
  /** The selection moved — a click, an arrow key, or an edit. */
  onSelectionChange?: (caret: number, collapsed: boolean) => void;
  /**
   * Runs before this component's own key handling. Preventing default claims
   * the key outright — that is how the caller keeps Enter for "send" and for
   * accepting a completion.
   */
  onKeyDown?: (e: React.KeyboardEvent) => void;
  /**
   * First refusal on a paste. Return true when the paste was consumed (e.g. it
   * was a file) and no text should be inserted. The event is always
   * defaultPrevented before this runs: a `contentEditable` must never take the
   * browser's own paste, which is HTML.
   */
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
  /** The terminal skin's inline completion, shown after the caret. */
  ghost?: string;
  /** Which completion track the ghost belongs to — mentions, or shortcodes. */
  ghostTestId?: string;
  /** Class applied to the ghost span; the composer inverts it when focused. */
  ghostClassName?: string;
}

/** True for the element that carries a rendered custom emoji. */
function isEmojiNode(node: Node): node is HTMLElement {
  return node.nodeType === ELEMENT_NODE && (node as HTMLElement).hasAttribute(EMOJI_TOKEN_ATTR);
}

/** True for the autosuggest span, which is never part of the value. */
function isGhostNode(node: Node): boolean {
  return node.nodeType === ELEMENT_NODE && (node as HTMLElement).hasAttribute(GHOST_ATTR);
}

/**
 * Build one custom emoji node.
 *
 * The token lives in an attribute, so serialization never depends on what the
 * node ended up rendering — a still-loading image, a resolved one, and the
 * `:shortcode:` fallback all produce the same wire text.
 */
function buildEmojiNode(token: string, shortcode: string, contentHash: string): HTMLElement {
  const span = document.createElement("span");
  span.setAttribute(EMOJI_TOKEN_ATTR, token);
  // The atom: the browser steps over it, selects it whole, and never places a
  // caret inside it.
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
      // Same degradation as `CustomEmojiImage`: a deleted or unreachable emoji
      // shows its shortcode rather than vanishing. The token attribute is
      // untouched, so the message still sends the emoji it was given.
      span.textContent = `:${shortcode}:`;
      span.classList.add("rich-input-emoji-fallback");
    });

  return span;
}

/** Replace the host's children with the projection of `value`. */
function project(host: HTMLElement, value: string): void {
  const fragment = document.createDocumentFragment();
  for (const run of richRuns(value)) {
    if (run.kind === "text") {
      fragment.appendChild(document.createTextNode(run.text));
      continue;
    }
    fragment.appendChild(buildEmojiNode(run.token, run.shortcode, run.contentHash));
  }
  // `white-space: pre-wrap` preserves a trailing newline in the string but does
  // not give it a line box, so Shift+Enter on the last line would look like
  // nothing happened. This is the filler the serializer skips.
  if (value === "" || value.endsWith("\n")) {
    fragment.appendChild(document.createElement("br"));
  }
  host.replaceChildren(fragment);
}

/**
 * The model offset of a DOM position.
 *
 * Measured by serializing everything before it, which means the mapping cannot
 * drift from the serializer: they are the same function.
 */
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

/** The DOM position of a model offset. The inverse of `offsetOf`. */
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
        // An offset landing inside a token snaps to just after it. Nothing in
        // the model ever produces one — the caret is only ever before or after
        // an emoji — so this is a floor, not a behaviour.
        if (remaining <= 0) {
          return { node: parent, offset: index + 1 };
        }
        continue;
      }
      if ((child as HTMLElement).tagName === "BR") {
        if (remaining <= 0) {
          return { node: parent, offset: index };
        }
        // Only a non-final `<br>` carries a newline; the filler carries none.
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
  const hostRef = useRef<HTMLDivElement>(null);
  // What the DOM currently holds, as wire text. `null` before the first
  // projection, so the mount always projects.
  const lastSerializedRef = useRef<string | null>(null);
  // Where to put the caret once the projection has been rebuilt.
  const pendingCaretRef = useRef<number | null>(null);

  // The callbacks and the current value are read through refs so the native
  // listeners below can be registered once instead of being torn down and
  // re-attached on every keystroke.
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
  }, []);

  /** Splice the model directly, bypassing whatever the browser would have done. */
  const replaceRange = useCallback((start: number, end: number, text: string) => {
    const current = valueRef.current;
    const next = current.slice(0, start) + text + current.slice(end);
    const caret = start + text.length;
    pendingCaretRef.current = caret;
    onChangeRef.current(next, caret);
  }, []);

  /** The current selection as model offsets, or null when it is not in here. */
  const selectionOffsets = useCallback((): { start: number; end: number } | null => {
    const host = hostRef.current;
    const selection = document.getSelection();
    if (!host || !selection || selection.rangeCount === 0) {
      return null;
    }
    const range = selection.getRangeAt(0);
    if (!host.contains(range.startContainer) || !host.contains(range.endContainer)) {
      return null;
    }
    return {
      start: offsetOf(host, range.startContainer, range.startOffset),
      end: offsetOf(host, range.endContainer, range.endOffset),
    };
  }, []);

  useImperativeHandle(
    ref,
    () => ({
      focus: () => hostRef.current?.focus(),
      blur: () => hostRef.current?.blur(),
      setSelection: (caret: number) => {
        hostRef.current?.focus();
        applySelection(caret);
      },
    }),
    [applySelection],
  );

  // Re-project when — and only when — the DOM and the value disagree.
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

  // The autosuggest, as a trailing non-editable node. Declared AFTER the
  // projection effect so it re-appends itself once the children have been
  // rebuilt. Appending at the end never moves a caret that is already placed,
  // which is what lets this live inside the editable at all — and the caller
  // only ever offers a ghost with the caret at end-of-value.
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
    // Recorded BEFORE the parent is told, so the projection effect knows the
    // DOM already says this and leaves the caret alone.
    lastSerializedRef.current = next;
    const selection = document.getSelection();
    const caret =
      selection && host.contains(selection.focusNode)
        ? offsetOf(host, selection.focusNode, selection.focusOffset)
        : next.length;
    onChangeRef.current(next, caret);
  }, []);

  // Deleting an emoji is decided against the model, not left to the engine.
  // Chrome, WebKit and Gecko each do something slightly different with a
  // Backspace next to a `contenteditable=false` element — delete it, select it
  // first, or step into it — and removing the token's own byte range makes the
  // keystroke identical everywhere.
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
      if (!offsets || offsets.start !== offsets.end) {
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

  // `selectionchange` is the only event a contentEditable has for "the caret
  // moved"; there is no `onSelect`. It fires on the document, so the guard that
  // the selection is actually in here is load-bearing.
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
      onSelectionChangeRef.current?.(
        offsetOf(host, selection.focusNode, selection.focusOffset),
        selection.isCollapsed,
      );
    };
    document.addEventListener("selectionchange", handler);
    return () => document.removeEventListener("selectionchange", handler);
  }, []);

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
      // Shift+Enter. Left to the browser this produces a `<div>` or a `<br>`
      // depending on the engine; done here it is one character in the model.
      if (e.key === "Enter") {
        const offsets = selectionOffsets();
        if (!offsets) {
          return;
        }
        e.preventDefault();
        replaceRange(offsets.start, offsets.end, "\n");
      }
    },
    [onKeyDown, replaceRange, selectionOffsets],
  );

  const handlePaste = useCallback(
    (e: React.ClipboardEvent) => {
      // Always: the browser's own paste into an editing host inserts HTML.
      e.preventDefault();
      if (onPaste?.(e)) {
        return;
      }
      const text = pastedPlainText(e.clipboardData);
      if (!text) {
        return;
      }
      const offsets = selectionOffsets();
      if (!offsets) {
        return;
      }
      replaceRange(offsets.start, offsets.end, text);
    },
    [onPaste, replaceRange, selectionOffsets],
  );

  // Copy and cut have to be written too, or an emoji leaves the composer as
  // whatever the image happened to serialize to. Serializing the selection with
  // the same walker is what makes token -> node -> token lossless through the
  // clipboard as well as through the editor.
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
      if (!writeSelection(e) || !offsets) {
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
