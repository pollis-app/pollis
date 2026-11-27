import { useSyncExternalStore } from "react";
import { View } from "react-native";
import { Text } from "../Text";
import { semantic, type as ty } from "../../theme/tokens";
import { useThemeRerender } from "../theme";
import { perfSummary, subscribePerf } from "../../lib/perfMarks";

/**
 * The chat timings (#2141) as text, for the chat-perf Maestro flow to read off the screen.
 * Mounted only in a build made with EXPO_PUBLIC_PERF_MARKS=1; never drawn for a user.
 */
export function PerfSummary() {
  useThemeRerender();
  const summary = useSyncExternalStore(subscribePerf, perfSummary);
  return (
    <View pointerEvents="none" style={{ position: "absolute", top: 0, start: 0 }}>
      {/* Not translated: a measurement readout, not UI copy. */}
      <Text testID="perf-summary" style={[ty.meta, { color: semantic.muted, fontSize: 8 }]}>
        {summary}
      </Text>
    </View>
  );
}
