import { Fragment, useMemo, useRef, useState } from "react";
import { PanResponder, Pressable, View, type LayoutChangeEvent } from "react-native";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Text } from "../Text";
import { useThemeRerender } from "../theme";
import { Badge, IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic, type as ty, fonts, r, space, layout, typeSize } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import type { Channel, ChannelCategory } from "../../types";
import {
  blockUnread,
  channelBlocks,
  collapsedVisible,
  stepCategory,
  stepChannel,
  type ChannelBlock,
} from "../../lib/channelLayout";
import { toggleCategoryCollapsed, useCollapsedCategories } from "../../lib/categoryCollapse";
import { isPendingCategory } from "../../hooks/queries/useChannelCategories";
import { ChannelRow } from "./ChannelRow";

type Block = ChannelBlock<Channel, ChannelCategory>;
type Kind = "channel" | "category";

interface Props {
  /** Text channels, display order (what the list shows). */
  channels: Channel[];
  /** Every channel, voice rooms included: what Edit mode arranges. */
  allChannels: Channel[];
  categories: ChannelCategory[];
  editing: boolean;
  selectedChannelId?: string | null;
  mutedChannels?: ReadonlySet<string>;
  onOpenChannel: (channel: Channel) => void;
  onPrefetch: (channelId: string) => void;
  onNewChannelIn: (category: ChannelCategory) => void;
  onCategoryMenu: (category: ChannelCategory, channelCount: number) => void;
  onNewCategory: () => void;
  onMoveChannel: (channelId: string, categoryId: string | null, position: number) => void;
  onMoveCategory: (categoryId: string, position: number) => void;
  /** A drag holds the list still: the scroll view stops scrolling while it is true. */
  onDraggingChange: (dragging: boolean) => void;
}

interface Drag {
  kind: Kind;
  id: string;
  dy: number;
}

/** Where a drop lands, and where its line is drawn (a y inside the list). */
type Drop =
  | { kind: "channel"; categoryId: string | null; position: number; lineY: number }
  | { kind: "category"; position: number; lineY: number };

/**
 * A group's channel list (#2031): the uncategorised channels first with no header, then each
 * category as a label with its channels under it; a collapsed category keeps only the open
 * channel. In Edit mode (admins, the iOS Mail / Discord pattern) every row gets a ≡ handle to
 * drag it, category labels gain "+" (a channel in it) and ⋯ (rename, delete), voice rooms show
 * so they can be arranged too, and a "New category" row ends the list. Nothing opens a channel
 * while editing.
 */
function GroupChannelListImpl(props: Props) {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const { editing, categories } = props;
  const collapsed = useCollapsedCategories();
  const blocks = useMemo(
    () => channelBlocks(editing ? props.allChannels : props.channels, categories),
    [editing, props.allChannels, props.channels, categories],
  );
  const layouts = useRef(new Map<string, { y: number; h: number }>());
  const [drag, setDragState] = useState<Drag | null>(null);
  // The gesture's callbacks read the drag through this, not through a state updater.
  const dragRef = useRef<Drag | null>(null);
  const setDrag = (d: Drag | null) => {
    dragRef.current = d;
    setDragState(d);
  };
  const unreadCounts = appStore.unreadCounts;

  // Rows in render order, for hit-testing a drag.
  const rows: { kind: Kind; id: string }[] = [];
  for (const b of blocks) {
    if (b.category) {
      rows.push({ kind: "category", id: b.category.id });
    }
    for (const c of b.channels) {
      rows.push({ kind: "channel", id: c.id });
    }
  }

  const keyOf = (kind: Kind, id: string) => `${kind}:${id}`;
  const record = (kind: Kind, id: string) => (e: LayoutChangeEvent) => {
    layouts.current.set(keyOf(kind, id), { y: e.nativeEvent.layout.y, h: e.nativeEvent.layout.height });
  };

  const dropFor = (d: Drag): Drop | null => {
    const start = layouts.current.get(keyOf(d.kind, d.id));
    if (!start) {
      return null;
    }
    const center = start.y + start.h / 2 + d.dy;
    const others = rows.filter((row) => !(row.kind === d.kind && row.id === d.id));
    const placed = others
      .map((row) => ({ ...row, box: layouts.current.get(keyOf(row.kind, row.id)) }))
      .filter((row): row is typeof row & { box: { y: number; h: number } } => !!row.box);
    const last = placed[placed.length - 1];
    const bottom = last ? last.box.y + last.box.h : 0;
    if (d.kind === "channel") {
      const next = placed.find((row) => row.box.y + row.box.h / 2 > center);
      // Blocks with the dragged channel lifted out: positions are the DS's.
      const lifted = blocks.map((b) => ({ ...b, channels: b.channels.filter((c) => c.id !== d.id) }));
      if (!next) {
        const end = lifted[lifted.length - 1];
        return { kind: "channel", categoryId: end.category?.id ?? null, position: end.channels.length, lineY: bottom };
      }
      if (next.kind === "channel") {
        const b = lifted.find((x) => x.channels.some((c) => c.id === next.id)) as Block;
        return {
          kind: "channel",
          categoryId: b.category?.id ?? null,
          position: b.channels.findIndex((c) => c.id === next.id),
          lineY: next.box.y,
        };
      }
      const j = lifted.findIndex((x) => x.category?.id === next.id);
      const prev = lifted[j - 1];
      return { kind: "channel", categoryId: prev.category?.id ?? null, position: prev.channels.length, lineY: next.box.y };
    }
    const labels = placed.filter((row) => row.kind === "category");
    const position = labels.filter((row) => row.box.y + row.box.h / 2 < center).length;
    const nextLabel = labels[position];
    return { kind: "category", position, lineY: nextLabel ? nextLabel.box.y : bottom };
  };

  const drop = drag ? dropFor(drag) : null;

  const start = (kind: Kind, id: string) => {
    setDrag({ kind, id, dy: 0 });
    props.onDraggingChange(true);
  };
  const move = (dy: number) => {
    const d = dragRef.current;
    if (d) {
      setDrag({ ...d, dy });
    }
  };
  const end = (commit: boolean) => {
    const d = dragRef.current;
    if (!d) {
      return;
    }
    const target = commit ? dropFor(d) : null;
    setDrag(null);
    props.onDraggingChange(false);
    if (!target) {
      return;
    }
    if (d.kind === "channel" && target.kind === "channel") {
      const from = blocks.find((b) => b.channels.some((c) => c.id === d.id));
      const same = (from?.category?.id ?? null) === target.categoryId;
      const at = from ? from.channels.findIndex((c) => c.id === d.id) : -1;
      if (!same || at !== target.position) {
        props.onMoveChannel(d.id, target.categoryId, target.position);
      }
    } else if (d.kind === "category" && target.kind === "category") {
      if (categories.findIndex((k) => k.id === d.id) !== target.position) {
        props.onMoveCategory(d.id, target.position);
      }
    }
  };

  const step = (kind: Kind, id: string, delta: -1 | 1) => {
    if (kind === "category") {
      const to = stepCategory(categories, id, delta);
      if (to !== null) {
        props.onMoveCategory(id, to);
      }
      return;
    }
    const s = stepChannel(blocks, id, delta);
    if (s) {
      props.onMoveChannel(id, s.categoryId, s.position);
    }
  };

  const handle = (kind: Kind, id: string, name: string) => (
    <DragHandle
      testID={`handle-${kind}-${id}`}
      label={t("channels:categories.move", { name })}
      moveUp={t("channels:categories.moveUp")}
      moveDown={t("channels:categories.moveDown")}
      onStart={() => start(kind, id)}
      onMove={move}
      onEnd={end}
      onStep={(delta) => step(kind, id, delta)}
    />
  );

  const dragStyle = (kind: Kind, id: string) =>
    drag && drag.kind === kind && drag.id === id
      ? {
          transform: [{ translateY: drag.dy }],
          zIndex: 2,
          backgroundColor: semantic.raised,
          borderRadius: r.sm,
        }
      : undefined;

  const renderChannel = (c: Channel) => {
    const unread = unreadCounts[c.id] ?? 0;
    const isVoice = c.channel_type === "voice";
    const selected = !editing && props.selectedChannelId === c.id;
    const isMuted = props.mutedChannels?.has(c.id) ?? false;
    const color = selected ? semantic.accent : isMuted ? semantic.muted : unread > 0 ? semantic.text : semantic.dim;
    const base = selected
      ? t("group.panel.channelSelected", { name: c.name })
      : unread > 0
        ? t("group.panel.channelUnread", { name: c.name, count: unread })
        : c.name;
    const label = isMuted ? `${base}, ${t("channels:channel.mutedHint")}` : base;
    const icon = isVoice ? <Icon.speak size={16} color={color} /> : <Icon.hash size={16} color={color} />;
    if (editing) {
      return (
        <View
          key={c.id}
          onLayout={record("channel", c.id)}
          testID={`row-edit-channel-${c.id}`}
          style={[
            { flexDirection: "row", alignItems: "center", gap: space.sm, minHeight: layout.touchMin, paddingStart: space.md },
            dragStyle("channel", c.id),
          ]}
        >
          <View accessible={false}>{icon}</View>
          <Text numberOfLines={1} style={{ flex: 1, fontFamily: fonts.medium, fontSize: typeSize.rowTitle, color: semantic.dim }}>
            {c.name}
          </Text>
          {handle("channel", c.id, c.name)}
        </View>
      );
    }
    return (
      <ChannelRow
        key={c.id}
        testID={isMuted ? `row-channel-${c.id}-muted` : `row-channel-${c.id}`}
        icon={icon}
        name={c.name}
        selected={selected}
        unread={unread > 0}
        muted={isMuted}
        accessibilityLabel={label}
        onPressIn={() => props.onPrefetch(c.id)}
        onPress={() => props.onOpenChannel(c)}
      />
    );
  };

  const renderLabel = (b: Block) => {
    const k = b.category as ChannelCategory;
    const isCollapsed = !editing && collapsed.has(k.id);
    const unread = blockUnread(b.channels, (id) => unreadCounts[id] ?? 0);
    if (editing && isPendingCategory(k.id)) {
      // Not on the DS yet: shown, but nothing acts on it until it lands.
      return (
        <View key={`label-${k.id}`} onLayout={record("category", k.id)} style={{ minHeight: layout.touchMin, justifyContent: "center", paddingStart: space.sm, marginTop: space.md }}>
          <Text numberOfLines={1} style={[ty.section, { color: semantic.muted }]}>
            {k.name}
          </Text>
        </View>
      );
    }
    if (editing) {
      return (
        <View
          key={`label-${k.id}`}
          onLayout={record("category", k.id)}
          testID={`row-edit-category-${k.id}`}
          style={[
            { flexDirection: "row", alignItems: "center", gap: space.xs, minHeight: layout.touchMin, paddingStart: space.sm, marginTop: space.md },
            dragStyle("category", k.id),
          ]}
        >
          <Text accessibilityRole="header" numberOfLines={1} style={[ty.section, { flex: 1 }]}>
            {k.name}
          </Text>
          <IconButton
            testID={`btn-category-new-channel-${k.id}`}
            accessibilityLabel={t("channels:categories.newChannelIn", { name: k.name })}
            icon={<Icon.plus size={18} color={semantic.dim} />}
            onPress={() => props.onNewChannelIn(k)}
          />
          <IconButton
            testID={`btn-category-menu-${k.id}`}
            accessibilityLabel={t("channels:categories.options", { name: k.name })}
            icon={<Icon.more size={18} color={semantic.dim} />}
            onPress={() => props.onCategoryMenu(k, b.channels.length)}
          />
          {handle("category", k.id, k.name)}
        </View>
      );
    }
    const spoken = isCollapsed
      ? unread > 0
        ? t("channels:categories.unread", { name: k.name, count: unread })
        : k.name
      : k.name;
    return (
      <Pressable
        key={`label-${k.id}`}
        testID={isCollapsed ? `row-category-${k.id}-collapsed` : `row-category-${k.id}`}
        accessibilityRole="button"
        accessibilityLabel={spoken}
        accessibilityHint={
          isCollapsed ? t("channels:categories.expand", { name: k.name }) : t("channels:categories.collapse", { name: k.name })
        }
        accessibilityState={{ expanded: !isCollapsed }}
        onPress={() => toggleCategoryCollapsed(k.id)}
        style={({ pressed }) => ({
          flexDirection: "row",
          alignItems: "center",
          gap: space.xs,
          minHeight: layout.touchMin,
          paddingStart: space.sm,
          paddingEnd: space.md,
          marginTop: space.md,
          borderRadius: r.sm,
          backgroundColor: pressed ? semantic.raised : "transparent",
        })}
      >
        {isCollapsed ? (
          <Icon.chevronRight size={16} color={semantic.muted} />
        ) : (
          <Icon.chevronDown size={16} color={semantic.muted} />
        )}
        <Text numberOfLines={1} style={[ty.section, { flex: 1 }]}>
          {k.name}
        </Text>
        {isCollapsed && unread > 0 ? <Badge>{unread > 99 ? "99+" : unread}</Badge> : null}
      </Pressable>
    );
  };

  return (
    <View>
      {editing ? (
        <Text style={[ty.meta, { paddingStart: space.sm, paddingBottom: space.sm }]}>
          {t("channels:categories.editHint")}
        </Text>
      ) : null}
      {blocks.map((b) => {
        if (!b.category) {
          return b.channels.map(renderChannel);
        }
        const k = b.category;
        const shown = !editing && collapsed.has(k.id) ? collapsedVisible(b.channels, [props.selectedChannelId]) : b.channels;
        // Flat siblings, so every row's layout y is in the list's own coordinates.
        return (
          <Fragment key={k.id}>
            {renderLabel(b)}
            {shown.map(renderChannel)}
          </Fragment>
        );
      })}
      {drop ? (
        <View
          pointerEvents="none"
          testID="drop-line"
          style={{
            position: "absolute",
            start: space.sm,
            end: space.sm,
            top: drop.lineY - 1,
            height: 2,
            borderRadius: 1,
            backgroundColor: semantic.accent,
          }}
        />
      ) : null}
      {editing ? (
        <Pressable
          testID="btn-new-category"
          accessibilityRole="button"
          accessibilityLabel={t("channels:categories.newCategory")}
          onPress={props.onNewCategory}
          style={({ pressed }) => ({
            flexDirection: "row",
            alignItems: "center",
            gap: space.md,
            minHeight: layout.touchMin,
            marginTop: space.lg,
            paddingHorizontal: space.md,
            borderRadius: r.sm,
            backgroundColor: pressed ? semantic.high : semantic.raised,
          })}
        >
          <Icon.folderPlus size={18} color={semantic.accent} />
          <Text style={[ty.button, { color: semantic.text }]}>{t("channels:categories.newCategory")}</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

// An observer: each row reads its own unread count from the store.
export const GroupChannelList = observer(GroupChannelListImpl);

/**
 * Edit mode's ≡: dragging it moves its row; nothing else on the row drags, so a scroll that starts
 * on a row is still a scroll. A screen reader gets Move up / Move down instead.
 */

function DragHandle({
  testID,
  label,
  moveUp,
  moveDown,
  onStart,
  onMove,
  onEnd,
  onStep,
}: {
  testID: string;
  label: string;
  moveUp: string;
  moveDown: string;
  onStart: () => void;
  onMove: (dy: number) => void;
  onEnd: (commit: boolean) => void;
  onStep: (delta: -1 | 1) => void;
}) {
  // Latest callbacks, read by the responder built once.
  const cb = useRef({ onStart, onMove, onEnd });
  cb.current = { onStart, onMove, onEnd };
  // The JS responder, not a native pan: it claims the touch on the handle at once and refuses
  // to hand it to the scroll view, so a drag is never turned into a scroll halfway.
  const responder = useMemo(
    () =>
      PanResponder.create({
        onStartShouldSetPanResponder: () => true,
        onMoveShouldSetPanResponder: () => true,
        onPanResponderTerminationRequest: () => false,
        onShouldBlockNativeResponder: () => true,
        onPanResponderGrant: () => cb.current.onStart(),
        onPanResponderMove: (_e, g) => cb.current.onMove(g.dy),
        onPanResponderRelease: () => cb.current.onEnd(true),
        onPanResponderTerminate: () => cb.current.onEnd(false),
      }),
    [],
  );
  return (
    <View
      {...responder.panHandlers}
      testID={testID}
      accessible
      accessibilityRole="adjustable"
      accessibilityLabel={label}
      accessibilityActions={[
        { name: "increment", label: moveDown },
        { name: "decrement", label: moveUp },
      ]}
      onAccessibilityAction={(e) => onStep(e.nativeEvent.actionName === "decrement" ? -1 : 1)}
      style={{ width: layout.touchMin, height: layout.touchMin, alignItems: "center", justifyContent: "center" }}
    >
      <Icon.dragHandle size={20} color={semantic.muted} />
    </View>
  );
}
