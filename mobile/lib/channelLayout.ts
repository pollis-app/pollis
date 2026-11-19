/**
 * A group's channel list as blocks (#2031): the uncategorised block first, with no header, then
 * one block per category in category order. The DS already returns channels in this order, so
 * the blocks are a stable bucketing of that list, never a sort.
 *
 * The moves below mirror what the DS does (pollis-delivery `channel_order`), so a drag can show
 * its result on the same frame and the server's answer replaces it.
 *
 * Mobile's `mobile/lib/channelLayout.ts` is this file, byte for byte (`mobile/tests/channel-layout.test.ts`).
 */

export interface LayoutChannel {
  id: string;
  category_id?: string | null;
}

export interface LayoutCategory {
  id: string;
  name: string;
}

export interface ChannelBlock<C extends LayoutChannel, K extends LayoutCategory> {
  /** `null` for the uncategorised block at the top. */
  category: K | null;
  channels: C[];
}

/** The category a channel renders under: `null` when it has none or names one not listed. */
export function blockOf<C extends LayoutChannel>(channel: C, categoryIds: ReadonlySet<string>): string | null {
  const id = channel.category_id ?? null;
  return id !== null && categoryIds.has(id) ? id : null;
}

/** The uncategorised block, then one block per category (empty ones included), channels in list order. */
export function channelBlocks<C extends LayoutChannel, K extends LayoutCategory>(
  channels: readonly C[],
  categories: readonly K[],
): ChannelBlock<C, K>[] {
  const ids = new Set(categories.map((k) => k.id));
  const top: ChannelBlock<C, K> = { category: null, channels: [] };
  const byId = new Map<string, ChannelBlock<C, K>>();
  const rest = categories.map((k) => {
    const block: ChannelBlock<C, K> = { category: k, channels: [] };
    byId.set(k.id, block);
    return block;
  });
  for (const ch of channels) {
    const key = blockOf(ch, ids);
    (key === null ? top : (byId.get(key) as ChannelBlock<C, K>)).channels.push(ch);
  }
  return [top, ...rest];
}

/** The flat list in display order, each channel's `category_id` as it renders. */
export function flattenBlocks<C extends LayoutChannel, K extends LayoutCategory>(
  blocks: readonly ChannelBlock<C, K>[],
): C[] {
  return blocks.flatMap((b) => b.channels.map((ch) => ({ ...ch, category_id: b.category ? b.category.id : null })));
}

/** `channelId` moved to `position` (clamped) in `categoryId`'s block (`null` = the top block). */
export function moveChannel<C extends LayoutChannel, K extends LayoutCategory>(
  channels: readonly C[],
  categories: readonly K[],
  channelId: string,
  categoryId: string | null,
  position: number,
): C[] {
  const blocks = channelBlocks(channels, categories);
  const moving = channels.find((c) => c.id === channelId);
  const target = blocks.find((b) => (b.category ? b.category.id : null) === categoryId);
  if (!moving || !target) {
    return [...channels];
  }
  for (const b of blocks) {
    b.channels = b.channels.filter((c) => c.id !== channelId);
  }
  const at = Math.max(0, Math.min(position, target.channels.length));
  target.channels.splice(at, 0, moving);
  return flattenBlocks(blocks);
}

/** `categoryId` moved to `position` (clamped) among the categories. */
export function moveCategory<K extends LayoutCategory>(categories: readonly K[], categoryId: string, position: number): K[] {
  const moving = categories.find((k) => k.id === categoryId);
  if (!moving) {
    return [...categories];
  }
  const rest = categories.filter((k) => k.id !== categoryId);
  rest.splice(Math.max(0, Math.min(position, rest.length)), 0, moving);
  return rest;
}

/** A category deleted: its channels join the end of the top block in their order; none is dropped. */
export function deleteCategory<C extends LayoutChannel, K extends LayoutCategory>(
  channels: readonly C[],
  categories: readonly K[],
  categoryId: string,
): { channels: C[]; categories: K[] } {
  const blocks = channelBlocks(channels, categories);
  const gone = blocks.find((b) => b.category?.id === categoryId);
  if (!gone) {
    return { channels: [...channels], categories: [...categories] };
  }
  blocks[0].channels.push(...gone.channels);
  const kept = blocks.filter((b) => b !== gone);
  return { channels: flattenBlocks(kept), categories: categories.filter((k) => k.id !== categoryId) };
}

/** A channel added at the bottom of its block. */
export function appendChannel<C extends LayoutChannel, K extends LayoutCategory>(
  channels: readonly C[],
  categories: readonly K[],
  channel: C,
): C[] {
  const blocks = channelBlocks(channels, categories);
  const ids = new Set(categories.map((k) => k.id));
  const key = blockOf(channel, ids);
  const target = blocks.find((b) => (b.category ? b.category.id : null) === key) ?? blocks[0];
  target.channels.push(channel);
  return flattenBlocks(blocks);
}

/** The sum of `unread` over a block's channels: a collapsed category's roll-up. */
export function blockUnread<C extends LayoutChannel>(channels: readonly C[], unread: (id: string) => number): number {
  return channels.reduce((sum, ch) => sum + unread(ch.id), 0);
}

/**
 * What a collapsed category still shows: only the channel you are in and a voice room you are
 * connected to, in list order.
 */
export function collapsedVisible<C extends LayoutChannel>(
  channels: readonly C[],
  keep: readonly (string | null | undefined)[],
): C[] {
  const ids = new Set(keep.filter((k): k is string => typeof k === "string" && k.length > 0));
  return channels.filter((ch) => ids.has(ch.id));
}

/**
 * Where a drop lands, as the DS's `position`: `index` is the slot among the target block's rows
 * as drawn (0 = above the first), counted with the dragged channel still in place. Dropping a
 * channel below itself in its own block lands one higher once it is lifted out.
 */
export function dropPosition<C extends LayoutChannel>(
  blockChannels: readonly C[],
  draggedId: string,
  index: number,
): number {
  const from = blockChannels.findIndex((c) => c.id === draggedId);
  if (from >= 0 && index > from) {
    return index - 1;
  }
  return index;
}

/**
 * One slot up (`-1`) or down (`1`) for a channel, the keyboard's and a screen reader's move: within
 * its block, or across the edge into the end of the block above / the top of the block below.
 * `null` at either end of the list.
 */
export function stepChannel<C extends LayoutChannel, K extends LayoutCategory>(
  blocks: readonly ChannelBlock<C, K>[],
  channelId: string,
  delta: -1 | 1,
): { categoryId: string | null; position: number } | null {
  const j = blocks.findIndex((b) => b.channels.some((c) => c.id === channelId));
  if (j < 0) {
    return null;
  }
  const key = (b: ChannelBlock<C, K>) => (b.category ? b.category.id : null);
  const i = blocks[j].channels.findIndex((c) => c.id === channelId);
  const to = i + delta;
  if (to >= 0 && to < blocks[j].channels.length) {
    return { categoryId: key(blocks[j]), position: to };
  }
  if (delta < 0 && j > 0) {
    return { categoryId: key(blocks[j - 1]), position: blocks[j - 1].channels.length };
  }
  if (delta > 0 && j < blocks.length - 1) {
    return { categoryId: key(blocks[j + 1]), position: 0 };
  }
  return null;
}

/** One slot up or down for a category; `null` at either end. */
export function stepCategory<K extends LayoutCategory>(categories: readonly K[], categoryId: string, delta: -1 | 1): number | null {
  const m = categories.findIndex((k) => k.id === categoryId);
  const to = m + delta;
  return m >= 0 && to >= 0 && to < categories.length ? to : null;
}
