// FlatList's `strictMode` (memoise the cell renderer on renderItem) exists in React Native's
// FlatList.js since 0.72 but is missing from its TypeScript definitions. The chat list uses it
// (#2168), so declare it where FlatListProps is defined.

import "react-native/Libraries/Lists/FlatList";

declare module "react-native/Libraries/Lists/FlatList" {
  interface FlatListProps<ItemT> {
    /** Memoise the item renderer so a list render with the same renderItem re-renders no cell. */
    strictMode?: boolean | undefined;
  }
}
