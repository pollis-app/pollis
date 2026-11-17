// The emoji sheet's category tabs (#1972), desktop's EmojiCategoryRail: navigation, not filtering. Pure.

/** A list item as far as the tabs care: a section header or a row inside one. */
export interface SectionedItem {
  type: "header" | "row";
  section: string;
}

export interface PickerTab {
  id: string;
  label: string;
}

/** One tab per section header, in list order. A search result list has no headers, so no tabs. */
export function pickerTabs<T extends SectionedItem & { label?: string }>(items: readonly T[]): PickerTab[] {
  const tabs: PickerTab[] = [];
  for (const item of items) {
    if (item.type === "header") {
      tabs.push({ id: item.section, label: item.label ?? item.section });
    }
  }
  return tabs;
}

/** Where a tab jumps to: its header's index, or -1 when the section is gone. */
export function sectionStartIndex(items: readonly SectionedItem[], section: string): number {
  return items.findIndex((item) => item.type === "header" && item.section === section);
}

