import React from "react";
import {
  Clock,
  Flag,
  Gamepad2,
  Hash,
  Lightbulb,
  Pizza,
  Plane,
  Smile,
  Sprout,
  Star,
} from "lucide-react";
import type { EmojiCategoryId } from "./emojiData";

/** A jump target in the rail. `custom-*` ids are per-group sections. */
export interface RailEntry {
  id: string;
  label: string;
}

interface EmojiCategoryRailProps {
  entries: readonly RailEntry[];
  activeId: string | null;
  onJump: (id: string) => void;
}

const ICON_PROPS = { size: 14, className: "size-[0.933rem] shrink-0" } as const;

/** Icon for a rail entry. Custom (per-group) sections all share the star. */
function iconFor(id: string): React.ReactNode {
  const byCategory: Partial<Record<EmojiCategoryId | "recent", React.ReactNode>> = {
    recent: <Clock {...ICON_PROPS} />,
    people: <Smile {...ICON_PROPS} />,
    nature: <Sprout {...ICON_PROPS} />,
    food: <Pizza {...ICON_PROPS} />,
    activity: <Gamepad2 {...ICON_PROPS} />,
    travel: <Plane {...ICON_PROPS} />,
    objects: <Lightbulb {...ICON_PROPS} />,
    symbols: <Hash {...ICON_PROPS} />,
    flags: <Flag {...ICON_PROPS} />,
  };
  return byCategory[id as EmojiCategoryId | "recent"] ?? <Star {...ICON_PROPS} />;
}

/**
 * The left-hand jump rail, as Discord has it: one icon per section, the active
 * one marked, clicking scrolls the list.
 *
 * It is navigation, not filtering — every section stays in the scroll list, so
 * a user who prefers to scroll never has to touch this.
 */
export const EmojiCategoryRail: React.FC<EmojiCategoryRailProps> = ({
  entries,
  activeId,
  onJump,
}) => {
  return (
    <nav
      data-testid="emoji-category-rail"
      aria-label="Emoji categories"
      className="flex flex-col shrink-0 gap-0.5 overflow-y-auto p-1 border-r border-line"
    >
      {entries.map((entry) => {
        const isActive = entry.id === activeId;
        return (
          <button
            key={entry.id}
            type="button"
            data-testid={`emoji-category-${entry.id}`}
            aria-label={entry.label}
            aria-current={isActive ? "true" : undefined}
            title={entry.label}
            onClick={() => onJump(entry.id)}
            className="icon-btn-sm"
            style={{
              color: isActive ? "var(--c-accent)" : undefined,
              background: isActive ? "var(--c-active)" : undefined,
            }}
          >
            {iconFor(entry.id)}
          </button>
        );
      })}
    </nav>
  );
};
