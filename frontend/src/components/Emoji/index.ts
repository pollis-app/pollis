/**
 * Custom + standard emoji (#848).
 *
 * `EmojiPickerButton` is the one entry point a caller normally needs: a trigger
 * plus its anchored, in-flow panel. `EmojiPicker` is exported for callers that
 * own their own trigger, and `EmojiText` / `CustomEmojiImage` are the render
 * half — turning `<:shortcode:hash>` tokens in message text into images.
 */

export { EmojiPicker } from "./EmojiPicker";
export { EmojiPickerButton } from "./EmojiPickerButton";
export { EmojiText } from "./EmojiText";
export { CustomEmojiImage } from "./CustomEmojiImage";
export { splitEmojiSegments, hasEmojiTokens, emojiTokenText } from "./emojiTokens";
export type { EmojiSegment } from "./emojiTokens";
