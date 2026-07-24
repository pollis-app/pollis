import React from "react";
import {
  View,
  Text,
  TextInput,
  Pressable,
  ScrollView,
  StyleProp,
  ViewStyle,
  TextStyle,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { useRouter } from "expo-router";
import { palette, semantic, type as ty, r, space, layout } from "../theme/tokens";
import { useTheme } from "./theme";
import { useLayoutClass } from "../hooks/useLayoutClass";
import { Icon } from "./icons";

/* ── Text ─────────────────────────────────────────────────────────── */
export function Txt({
  children,
  style,
  numberOfLines,
}: {
  children: React.ReactNode;
  style?: StyleProp<TextStyle>;
  numberOfLines?: number;
}) {
  return (
    <Text
      numberOfLines={numberOfLines}
      style={[{ fontFamily: ty.body.fontFamily, color: semantic.ink }, style]}
    >
      {children}
    </Text>
  );
}

/* ── Screen ───────────────────────────────────────────────────────── */
export function Screen({
  children,
  testID,
  centered,
}: {
  children: React.ReactNode;
  // Each route sets `screen-<route>` here so e2e flows have one stable root
  // anchor per screen. Inert in production.
  testID?: string;
  // Single-column screens (auth, self, forms) set this. On `regular` (iPad)
  // width it constrains + centers the content to a readable column; on
  // `compact` (phones, narrow panes) it is a no-op — children render exactly as
  // today, with no wrapper, so the phone tree is byte-for-byte unchanged.
  centered?: boolean;
}) {
  // Subscribe to the accent so the whole subtree re-renders (and the token
  // getters resolve to the new color) when it changes.
  useTheme();
  const cls = useLayoutClass();
  const centerBody = centered && cls === "regular";
  return (
    <SafeAreaView
      testID={testID}
      style={{ flex: 1, backgroundColor: palette.bg }}
      edges={["top", "bottom"]}
    >
      {centerBody ? (
        <View
          style={{
            flex: 1,
            width: "100%",
            maxWidth: layout.readableMaxWidth,
            alignSelf: "center",
          }}
        >
          {children}
        </View>
      ) : (
        children
      )}
    </SafeAreaView>
  );
}

/* ── Diamond (crumb tick / verified notch) ────────────────────────── */
export function Diamond({
  size = 7,
  fill = true,
}: {
  size?: number;
  fill?: boolean;
}) {
  return (
    <View
      style={{
        width: size,
        height: size,
        transform: [{ rotate: "45deg" }],
        backgroundColor: fill ? semantic.accent : "transparent",
        borderWidth: fill ? 0 : 1,
        borderColor: semantic.ink2,
      }}
    />
  );
}

/* ── Crumb ────────────────────────────────────────────────────────── */
export function Crumb({
  segs,
  end,
  testID,
}: {
  segs: { label: string; leaf?: boolean }[];
  end?: string;
  // Optional passive anchor; some routes prefer `screen-*` on <Screen>, but
  // the crumb is a convenient stable header target too.
  testID?: string;
}) {
  return (
    <View
      testID={testID}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.sm,
        paddingTop: space.xs,
        paddingBottom: space.lg,
        paddingHorizontal: space.xxl,
      }}
    >
      <Diamond size={7} />
      <View style={{ flexDirection: "row", alignItems: "center" }}>
        {segs.map((s, i) => (
          <React.Fragment key={i}>
            {i > 0 && (
              <Text style={[ty.crumb, { color: semantic.mute2, marginHorizontal: 4 }]}>
                ·
              </Text>
            )}
            <Text
              style={[ty.crumb, { color: s.leaf ? semantic.ink : semantic.ink2 }]}
            >
              {s.label}
            </Text>
          </React.Fragment>
        ))}
      </View>
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }} />
      {end ? (
        <Text style={[ty.crumb, { color: semantic.mute }]}>{end}</Text>
      ) : null}
    </View>
  );
}

/* ── Section title ────────────────────────────────────────────────── */
export function SectionTitle({
  children,
  right,
}: {
  children: string;
  right?: React.ReactNode;
}) {
  return (
    <View
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.md,
        paddingHorizontal: space.xxl,
        paddingTop: space.xxl,
        paddingBottom: space.sm,
      }}
    >
      <Text style={[ty.label, { color: semantic.ink2 }]}>{children}</Text>
      <View style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }} />
      {right}
    </View>
  );
}

/* ── Chip ─────────────────────────────────────────────────────────── */
export function Chip({
  children,
  variant = "default",
  onPress,
  style,
  testID,
  accessibilityLabel,
}: {
  children: React.ReactNode;
  variant?: "default" | "on" | "solid" | "subtle";
  onPress?: () => void;
  style?: StyleProp<ViewStyle>;
  // Set when the chip is an interactive affordance (accept/decline/toggle),
  // e.g. `chip-accent-mint`. Harmless when the chip is purely decorative.
  testID?: string;
  accessibilityLabel?: string;
}) {
  const border =
    variant === "on"
      ? semantic.accent
      : variant === "solid"
        ? semantic.accent
        : variant === "subtle"
          ? "transparent"
          : semantic.hairStrong;
  const bg =
    variant === "solid"
      ? semantic.accent
      : variant === "subtle"
        ? semantic.hairSoft
        : "transparent";
  const fg =
    variant === "on"
      ? semantic.accent
      : variant === "solid"
        ? palette.bg
        : semantic.ink2;
  return (
    <Pressable
      onPress={onPress}
      testID={testID}
      accessibilityLabel={
        accessibilityLabel ??
        (typeof children === "string" ? children : undefined)
      }
      style={[
        {
          flexDirection: "row",
          alignItems: "center",
          gap: space.xs,
          paddingVertical: 4,
          paddingHorizontal: space.md,
          borderWidth: 1,
          borderColor: border,
          backgroundColor: bg,
          borderRadius: r.sm,
        },
        style,
      ]}
    >
      {typeof children === "string" ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 11,
            letterSpacing: 0.9,
            color: fg,
          }}
        >
          {children}
        </Text>
      ) : (
        children
      )}
    </Pressable>
  );
}

/* ── Button ───────────────────────────────────────────────────────── */
export function Button({
  children,
  variant = "default",
  full,
  onPress,
  icon,
  iconRight,
  disabled,
  align = "center",
  testID,
  accessibilityLabel,
}: {
  children: string;
  variant?: "default" | "primary" | "subtle" | "danger";
  full?: boolean;
  onPress?: () => void;
  icon?: React.ReactNode;
  iconRight?: React.ReactNode;
  disabled?: boolean;
  align?: "center" | "left";
  // `btn-<name>` for e2e flows; accessibilityLabel defaults to the button's
  // text label when not given explicitly.
  testID?: string;
  accessibilityLabel?: string;
}) {
  const primary = variant === "primary";
  const danger = variant === "danger";
  const subtle = variant === "subtle";
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel ?? children}
      accessibilityState={{ disabled: !!disabled }}
      style={{
        opacity: disabled ? 0.45 : 1,
        flexDirection: "row",
        alignItems: "center",
        justifyContent: align === "left" ? "flex-start" : "center",
        gap: space.sm,
        paddingVertical: full ? space.xl : space.lg,
        paddingHorizontal: space.xl,
        borderWidth: 1,
        borderColor: primary
          ? semantic.accent
          : danger
            ? "rgba(196,106,46,.4)"
            : subtle
              ? "transparent"
              : semantic.hairStrong,
        backgroundColor: primary
          ? semantic.accent
          : subtle
            ? semantic.hairSoft
            : "transparent",
        borderRadius: r.sm,
        width: full ? "100%" : undefined,
      }}
    >
      {icon}
      <Text
        style={{
          fontFamily: primary ? ty.h1.fontFamily : ty.rowN.fontFamily,
          fontSize: full ? 15 : 14,
          letterSpacing: 0.3,
          color: primary
            ? palette.bg
            : danger
              ? semantic.danger
              : semantic.ink,
        }}
      >
        {children}
      </Text>
      {iconRight}
    </Pressable>
  );
}

/* ── Avatar ───────────────────────────────────────────────────────── */
export function Avatar({
  size = "md",
  variant = "default",
  style,
}: {
  // `label` (initials) is still accepted so existing call sites typecheck, but
  // it's no longer rendered: with no profile-image support yet, the avatar
  // fallback is the lucide user icon per design.
  label?: string;
  size?: "sm" | "md" | "lg";
  variant?: "default" | "amber" | "solid";
  style?: StyleProp<ViewStyle>;
}) {
  const dim = size === "sm" ? 24 : size === "lg" ? 48 : 32;
  const iconSize = Math.round(dim * 0.55);
  const iconColor =
    variant === "solid"
      ? palette.bg
      : variant === "amber"
        ? semantic.accent
        : semantic.ink2;
  return (
    <View
      style={[
        {
          width: dim,
          height: dim,
          borderRadius: r.sm,
          borderWidth: 1,
          alignItems: "center",
          justifyContent: "center",
          borderColor:
            variant === "default" ? semantic.hairStrong : semantic.accent,
          backgroundColor:
            variant === "solid" ? semantic.accent : "transparent",
        },
        style,
      ]}
    >
      <Icon.user size={iconSize} color={iconColor} />
    </View>
  );
}

/* ── ListRow ──────────────────────────────────────────────────────── */
export function ListRow({
  glyph,
  name,
  sub,
  end,
  selected,
  minHeight = 56,
  onPress,
  nameStyle,
  testID,
  accessibilityLabel,
}: {
  glyph?: React.ReactNode;
  name: React.ReactNode;
  sub?: React.ReactNode;
  end?: React.ReactNode;
  selected?: boolean;
  minHeight?: number;
  onPress?: () => void;
  nameStyle?: StyleProp<TextStyle>;
  // `row-<kind>-<id>` for list rows so flows can target a specific record.
  testID?: string;
  accessibilityLabel?: string;
}) {
  return (
    <Pressable
      onPress={onPress}
      testID={testID}
      accessibilityLabel={
        accessibilityLabel ?? (typeof name === "string" ? name : undefined)
      }
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight,
        paddingVertical: space.lg,
        paddingHorizontal: selected ? space.lg : space.xxl,
        marginHorizontal: selected ? space.xs : 0,
        backgroundColor: selected ? semantic.hairSoft : "transparent",
        borderRadius: selected ? r.sm : 0,
        borderBottomWidth: selected ? 0 : 1,
        borderBottomColor: semantic.hairSoft,
      }}
    >
      {glyph !== undefined && (
        <View style={{ width: 22, alignItems: "center" }}>{glyph}</View>
      )}
      <View style={{ flex: 1, minWidth: 0 }}>
        {typeof name === "string" ? (
          <Text
            style={[
              { fontFamily: ty.rowN.fontFamily, fontSize: 15, color: semantic.ink },
              nameStyle,
            ]}
          >
            {name}
          </Text>
        ) : (
          name
        )}
        {sub !== undefined &&
          (typeof sub === "string" ? (
            <Text
              numberOfLines={1}
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.mute,
                marginTop: 2,
              }}
            >
              {sub}
            </Text>
          ) : (
            <View style={{ marginTop: 2 }}>{sub}</View>
          ))}
      </View>
      {end !== undefined && (
        <View
          style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}
        >
          {end}
        </View>
      )}
    </Pressable>
  );
}

/* ── Badge ────────────────────────────────────────────────────────── */
export function Badge({ children }: { children: React.ReactNode }) {
  return (
    <View
      style={{
        minWidth: 18,
        height: 18,
        paddingHorizontal: space.xs,
        backgroundColor: semantic.accent,
        borderRadius: r.sm,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <Text
        style={{
          fontFamily: ty.h1.fontFamily,
          fontSize: 10,
          color: palette.bg,
        }}
      >
        {children}
      </Text>
    </View>
  );
}

/* ── Field ────────────────────────────────────────────────────────── */
export function Field({
  value,
  onChangeText,
  placeholder,
  icon,
  trailing,
  amber,
  editable = true,
  secureTextEntry,
  keyboardType,
  testID,
  accessibilityLabel,
}: {
  value?: string;
  onChangeText?: (v: string) => void;
  placeholder?: string;
  icon?: React.ReactNode;
  trailing?: React.ReactNode;
  amber?: boolean;
  editable?: boolean;
  secureTextEntry?: boolean;
  keyboardType?: "default" | "email-address" | "number-pad";
  // `input-<name>` on the underlying TextInput; accessibilityLabel comes from
  // the caller's field label (Field renders no label of its own).
  testID?: string;
  accessibilityLabel?: string;
}) {
  return (
    <View
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.sm,
        borderWidth: 1,
        borderColor: amber ? semantic.accent : semantic.hairStrong,
        backgroundColor: amber ? semantic.hairSoft : semantic.fieldBg,
        paddingVertical: space.md,
        paddingHorizontal: space.lg,
        borderRadius: r.sm,
      }}
    >
      {icon}
      <TextInput
        testID={testID}
        accessibilityLabel={accessibilityLabel}
        value={value}
        onChangeText={onChangeText}
        placeholder={placeholder}
        placeholderTextColor={semantic.mute2}
        editable={editable}
        secureTextEntry={secureTextEntry}
        keyboardType={keyboardType}
        autoCapitalize="none"
        style={{
          flex: 1,
          fontFamily: ty.body.fontFamily,
          fontSize: 14,
          color: semantic.ink,
          padding: 0,
        }}
      />
      {trailing}
    </View>
  );
}

/* ── Toggle ───────────────────────────────────────────────────────── */
export function Toggle({
  on,
  onPress,
  testID,
  accessibilityLabel,
}: {
  on?: boolean;
  onPress?: () => void;
  // `toggle-<name>`; exposes switch semantics so e2e + a11y read the state.
  testID?: string;
  accessibilityLabel?: string;
}) {
  return (
    <Pressable
      onPress={onPress}
      testID={testID}
      accessibilityRole="switch"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ checked: !!on }}
      style={{
        width: 36,
        height: 20,
        borderWidth: 1,
        borderColor: on ? semantic.accent : semantic.hairStrong,
        borderRadius: r.sm,
        justifyContent: "center",
      }}
    >
      <View
        style={{
          position: "absolute",
          top: 2,
          left: on ? 18 : 2,
          width: 14,
          height: 14,
          borderRadius: 1,
          backgroundColor: on ? semantic.accent : semantic.mute,
        }}
      />
    </Pressable>
  );
}

/* ── Card ─────────────────────────────────────────────────────────── */
export function Card({
  children,
  style,
}: {
  children: React.ReactNode;
  style?: StyleProp<ViewStyle>;
}) {
  return (
    <View
      style={[
        {
          borderWidth: 1,
          borderColor: semantic.hairStrong,
          backgroundColor: semantic.fieldBg,
          padding: 16,
          borderRadius: r.lg,
        },
        style,
      ]}
    >
      {children}
    </View>
  );
}

/* ── Bottom action zone ───────────────────────────────────────────── */
export function BottomAction({ children }: { children: React.ReactNode }) {
  return (
    <View
      style={{
        gap: space.md,
        paddingVertical: space.xl,
        paddingHorizontal: space.xxl,
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
        backgroundColor: palette.bg,
      }}
    >
      {children}
    </View>
  );
}

/* ── Context strip (bottom back bar on pushed screens) ────────────── */
export function Ctx({
  cr,
  name,
  actions,
  testID,
  hideBack,
}: {
  cr?: string;
  name: React.ReactNode;
  actions?: React.ReactNode;
  // Optional anchor on the context strip container. The back Pressable always
  // carries `btn-back` for navigation flows.
  testID?: string;
  // Two-pane embedded mode (issue #622): the conversation sits in a pane with
  // nothing to pop, so the back affordance is omitted. Default keeps back.
  hideBack?: boolean;
}) {
  const router = useRouter();
  return (
    <View
      testID={testID}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight: 52,
        paddingVertical: space.md,
        paddingHorizontal: space.xl,
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
        backgroundColor: palette.bg,
      }}
    >
      {hideBack ? null : (
        <Pressable
          onPress={() => router.back()}
          testID="btn-back"
          accessibilityRole="button"
          accessibilityLabel="Back"
          style={{
            width: 38,
            height: 38,
            alignItems: "center",
            justifyContent: "center",
            borderWidth: 1,
            borderColor: semantic.hair,
            borderRadius: r.sm,
          }}
        >
          <Icon.arrowLeft />
        </Pressable>
      )}
      <View style={{ flex: 1, minWidth: 0 }}>
        {cr ? (
          <Text style={[ty.label, { fontSize: 9, letterSpacing: 1.8 }]}>
            {cr}
          </Text>
        ) : null}
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: space.xs,
            marginTop: 1,
          }}
        >
          {typeof name === "string" ? (
            <Text
              style={{
                fontFamily: ty.rowN.fontFamily,
                fontSize: 15,
                color: semantic.ink,
              }}
            >
              {name}
            </Text>
          ) : (
            name
          )}
        </View>
      </View>
      {actions ? (
        <View style={{ flexDirection: "row", alignItems: "center" }}>
          {actions}
        </View>
      ) : null}
    </View>
  );
}

export function CtxAct({
  icon,
  onPress,
  testID,
  accessibilityLabel,
}: {
  icon: React.ReactNode;
  onPress?: () => void;
  // `btn-<name>` — bottom command actions are load-bearing in flows.
  testID?: string;
  accessibilityLabel?: string;
}) {
  return (
    <Pressable
      onPress={onPress}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel}
      style={{
        width: 38,
        height: 38,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {icon}
    </Pressable>
  );
}

/* ── Scrollable body ──────────────────────────────────────────────── */
export function Body({ children }: { children: React.ReactNode }) {
  return (
    <ScrollView
      style={{ flex: 1 }}
      contentContainerStyle={{ paddingBottom: space.xl }}
      keyboardShouldPersistTaps="handled"
    >
      {children}
    </ScrollView>
  );
}
