import React from "react";
import { Text as RNText, type TextProps } from "react-native";

// The app's Text: React Native's, with Hangul kept whole on iOS (#1941).
// iOS otherwise breaks Korean lines between syllables inside a word; Android's
// line breaker already keeps words together. The strategy only changes how
// Hangul wraps, so it applies in every UI language, including to Korean in
// messages. Import Text from here, never from react-native (a test enforces it).
export function Text(props: TextProps & { ref?: React.Ref<RNText> }) {
  return <RNText lineBreakStrategyIOS="hangul-word" {...props} />;
}
