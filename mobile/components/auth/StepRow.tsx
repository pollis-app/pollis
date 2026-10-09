import { Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty } from "../../theme/tokens";

export type StepState = "done" | "active" | "error" | "todo";

const MARK = 20;

/**
 * One line of the Initializing checklist: a mark, the step name and its
 * status. The mark's SHAPE carries the state (filled check, half ring,
 * alert, empty ring) so it never rests on colour alone; the row speaks as
 * one element ("Keys loaded, OK").
 */
export function StepRow({ name, status, state }: { name: string; status: string; state: StepState }) {
  const mark =
    state === "done" ? (
      <View
        style={{
          width: MARK,
          height: MARK,
          borderRadius: MARK / 2,
          backgroundColor: semantic.accent,
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <Icon.check size={13} color={semantic.onAccent} />
      </View>
    ) : state === "error" ? (
      <Icon.alert size={MARK} color={semantic.danger} />
    ) : (
      <View
        style={{
          width: MARK,
          height: MARK,
          borderRadius: MARK / 2,
          borderWidth: 2,
          borderColor: state === "active" ? semantic.accent : semantic.edge,
          borderTopColor: semantic.edge,
        }}
      />
    );
  return (
    <View
      accessible
      accessibilityLabel={`${name}, ${status}`}
      style={{ flexDirection: "row", alignItems: "center", gap: 12, minHeight: 32 }}
    >
      {mark}
      <Text style={[ty.secondary, { flex: 1, color: state === "todo" ? semantic.muted : semantic.text }]}>
        {name}
      </Text>
      <Text style={[ty.meta, { color: state === "done" ? semantic.dim : semantic.muted }]}>{status}</Text>
    </View>
  );
}
