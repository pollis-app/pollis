import React, { useCallback, useEffect, useRef, useState } from "react";
import type { ChannelBlock, LayoutCategory, LayoutChannel } from "../../utils/channelLayout";
import { dropPosition, stepCategory, stepChannel } from "../../utils/channelLayout";
import type { DropEdge, SidebarRowDrag } from "./SidebarRow";

/*
 * Dragging a group's channels and category labels in the sidebar (#2031). Admins only: the caller
 * passes `enabled: false` for everyone else and no row gets any drag wiring.
 *
 * Pointer events, not HTML5 drag and drop: the webview's native drop handling is reserved for
 * files, and a pointer drag works the same under every OS webview and in Playwright. A press that
 * moves less than DRAG_SLOP is a click; past it, the row follows as a drag, the slot it would land
 * in is drawn as a line, and release sends one move. Escape cancels. Alt+↑ / Alt+↓ on a focused
 * row move it one slot, for the keyboard.
 */

const DRAG_SLOP_PX = 5;

type Kind = "channel" | "category";

interface Dragging {
  kind: Kind;
  id: string;
}

/** A slot: for a channel, a block (null = top) and an index among its rows; for a category, an index. */
type Slot =
  | { kind: "channel"; categoryId: string | null; index: number }
  | { kind: "category"; index: number };

interface ArrangeHandlers {
  onMoveChannel: (channelId: string, categoryId: string | null, position: number) => void;
  onMoveCategory: (categoryId: string, position: number) => void;
}

interface Options<C extends LayoutChannel, K extends LayoutCategory> extends ArrangeHandlers {
  groupId: string;
  enabled: boolean;
  blocks: ChannelBlock<C, K>[];
}

const blockKey = (b: ChannelBlock<LayoutChannel, LayoutCategory>) => (b.category ? b.category.id : null);

export function useSidebarArrange<C extends LayoutChannel, K extends LayoutCategory>({
  groupId,
  enabled,
  blocks,
  onMoveChannel,
  onMoveCategory,
}: Options<C, K>) {
  const [dragging, setDragging] = useState<Dragging | null>(null);
  const [slot, setSlot] = useState<Slot | null>(null);
  const blocksRef = useRef(blocks);
  blocksRef.current = blocks;
  const handlersRef = useRef({ onMoveChannel, onMoveCategory });
  handlersRef.current = { onMoveChannel, onMoveCategory };

  const categories = blocks.slice(1).map((b) => b.category as K);

  /** The slot under the pointer, from the row it is over and which half. */
  const slotAt = useCallback(
    (drag: Dragging, x: number, y: number): Slot | null => {
      const el = document.elementFromPoint(x, y)?.closest<HTMLElement>(`[data-arrange-group="${groupId}"]`);
      if (!el) {
        return null;
      }
      const rect = el.getBoundingClientRect();
      const lower = y > rect.top + rect.height / 2;
      const kind = el.dataset.arrangeKind as Kind;
      const id = el.dataset.arrangeId ?? "";
      const bs = blocksRef.current;
      if (drag.kind === "channel") {
        if (kind === "channel") {
          const block = bs.find((b) => b.channels.some((c) => c.id === id));
          if (!block) {
            return null;
          }
          const i = block.channels.findIndex((c) => c.id === id);
          return { kind: "channel", categoryId: blockKey(block), index: lower ? i + 1 : i };
        }
        const j = bs.findIndex((b) => b.category?.id === id);
        if (j < 0) {
          return null;
        }
        // The lower half of a label is the top of its category; the upper half the end of the block above.
        return lower
          ? { kind: "channel", categoryId: id, index: 0 }
          : { kind: "channel", categoryId: blockKey(bs[j - 1]), index: bs[j - 1].channels.length };
      }
      const m = bs.slice(1).findIndex((b) => b.category?.id === id);
      if (kind === "category" && m >= 0) {
        return { kind: "category", index: lower ? m + 1 : m };
      }
      // Over a channel: the top block sorts before every category, a category's rows after it.
      const block = bs.find((b) => b.channels.some((c) => c.id === id));
      if (!block) {
        return null;
      }
      const k = bs.slice(1).findIndex((b) => b === block);
      return { kind: "category", index: k < 0 ? 0 : k + 1 };
    },
    [groupId],
  );

  const commit = useCallback((drag: Dragging, s: Slot) => {
    const bs = blocksRef.current;
    if (drag.kind === "channel" && s.kind === "channel") {
      const target = bs.find((b) => blockKey(b) === s.categoryId);
      if (!target) {
        return;
      }
      const position = dropPosition(target.channels, drag.id, s.index);
      const source = bs.find((b) => b.channels.some((c) => c.id === drag.id));
      const from = source ? source.channels.findIndex((c) => c.id === drag.id) : -1;
      if (source === target && from === position) {
        return;
      }
      handlersRef.current.onMoveChannel(drag.id, s.categoryId, position);
    } else if (drag.kind === "category" && s.kind === "category") {
      const cats = bs.slice(1).map((b) => ({ id: (b.category as LayoutCategory).id }));
      const position = dropPosition(cats, drag.id, s.index);
      if (cats.findIndex((k) => k.id === drag.id) === position) {
        return;
      }
      handlersRef.current.onMoveCategory(drag.id, position);
    }
  }, []);

  // One pointer gesture at a time, tracked on the window once it starts.
  const pending = useRef<{ drag: Dragging; x: number; y: number; started: boolean; slot: Slot | null } | null>(null);

  useEffect(() => {
    if (!enabled) {
      return;
    }
    const swallowClick = (e: MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      window.removeEventListener("click", swallowClick, true);
    };
    const end = () => {
      pending.current = null;
      setDragging(null);
      setSlot(null);
    };
    const onMove = (e: PointerEvent) => {
      const p = pending.current;
      if (!p) {
        return;
      }
      if (!p.started) {
        if (Math.hypot(e.clientX - p.x, e.clientY - p.y) < DRAG_SLOP_PX) {
          return;
        }
        p.started = true;
        setDragging(p.drag);
      }
      e.preventDefault();
      p.slot = slotAt(p.drag, e.clientX, e.clientY);
      setSlot(p.slot);
    };
    const onUp = () => {
      const p = pending.current;
      if (!p) {
        return;
      }
      if (p.started) {
        // The release would also click the row under it: a drag is not a navigation.
        window.addEventListener("click", swallowClick, true);
        window.setTimeout(() => window.removeEventListener("click", swallowClick, true), 0);
        if (p.slot) {
          commit(p.drag, p.slot);
        }
      }
      end();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && pending.current?.started) {
        e.preventDefault();
        e.stopImmediatePropagation();
        end();
      }
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", end);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", end);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [enabled, slotAt, commit]);

  /** Alt+↑ / Alt+↓: one slot up or down, across block edges for a channel. */
  const keyMove = useCallback(
    (kind: Kind, id: string, delta: -1 | 1) => {
      const bs = blocksRef.current;
      if (kind === "category") {
        const to = stepCategory(bs.slice(1).map((b) => b.category as K), id, delta);
        if (to !== null) {
          handlersRef.current.onMoveCategory(id, to);
        }
        return;
      }
      const step = stepChannel(bs, id, delta);
      if (step) {
        handlersRef.current.onMoveChannel(id, step.categoryId, step.position);
      }
    },
    [],
  );

  /** Where the slot's line is drawn: before or after one rendered row. */
  const lineFor = (kind: Kind, id: string, visible: (channelId: string) => boolean): DropEdge => {
    if (!dragging || !slot) {
      return null;
    }
    if (slot.kind === "channel") {
      const block = blocks.find((b) => blockKey(b) === slot.categoryId);
      if (!block) {
        return null;
      }
      const shown = block.channels.filter((c) => visible(c.id));
      const at = block.channels[slot.index];
      if (at && visible(at.id)) {
        return kind === "channel" && id === at.id ? "before" : null;
      }
      const last = shown[shown.length - 1];
      if (last && slot.index > 0) {
        return kind === "channel" && id === last.id ? "after" : null;
      }
      // An empty or collapsed block: the line sits under its label (the top block has none).
      if (block.category) {
        return kind === "category" && id === block.category.id ? "after" : null;
      }
      const first = blocks[1]?.category;
      return first && kind === "category" && id === first.id ? "before" : null;
    }
    const at = categories[slot.index];
    if (at) {
      return kind === "category" && id === at.id ? "before" : null;
    }
    // Past the last category: under the last row drawn.
    const lastBlock = blocks[blocks.length - 1];
    const shown = lastBlock.channels.filter((c) => visible(c.id));
    const last = shown[shown.length - 1];
    if (last) {
      return kind === "channel" && id === last.id ? "after" : null;
    }
    return lastBlock.category && kind === "category" && id === lastBlock.category.id ? "after" : null;
  };

  /** The drag wiring for one row, or `undefined` when arranging is off. */
  const rowDrag = (kind: Kind, id: string, visible: (channelId: string) => boolean): SidebarRowDrag | undefined => {
    if (!enabled) {
      return undefined;
    }
    return {
      rowProps: {
        "data-arrange-group": groupId,
        "data-arrange-kind": kind,
        "data-arrange-id": id,
        onPointerDown: (e: React.PointerEvent) => {
          if (e.button !== 0 || (e.target as HTMLElement).closest("[data-arrange-ignore]")) {
            return;
          }
          pending.current = { drag: { kind, id }, x: e.clientX, y: e.clientY, started: false, slot: null };
        },
      },
      isDragging: dragging?.kind === kind && dragging.id === id,
      dropEdge: lineFor(kind, id, visible),
      onKeyDown: (e: React.KeyboardEvent) => {
        if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
          e.preventDefault();
          e.stopPropagation();
          keyMove(kind, id, e.key === "ArrowUp" ? -1 : 1);
        }
      },
    };
  };

  return { rowDrag, isDragging: dragging !== null };
}
