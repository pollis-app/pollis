import React, { useState } from "react";
import {
  View,
  Text,
  TextInput,
  Pressable,
  ScrollView,
  KeyboardAvoidingView,
  Platform,
  StyleProp,
  ViewStyle,
  TextStyle,
  TextInputProps,
  AccessibilityRole,
} from "react-native";
import { SafeAreaView, useSafeAreaInsets } from "react-native-safe-area-context";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { semantic, type as ty, fonts, r, space, layout, currentTheme } from "../theme/tokens";
import { buttonColors } from "../theme/button";
import { useTheme } from "./theme";
import { useLayoutClass } from "../hooks/useLayoutClass";
import { useAndroidKeyboardInset } from "../hooks/useAndroidKeyboardInset";
import { activeLocale } from "../i18n";
import { Icon } from "./icons";

// Primitives for the redesigned app (design: Main/Chat/Messages/Actions/You
// .dc.html). Rules every primitive follows: 44pt minimum touch targets; one
// spoken label per control; start/end edges, never left/right; no coloured
// stripe on one edge of a rounded box; Text keeps system font scaling.

type TypeKey =
  | "display"
  | "title"
  | "heading"
  | "body"
  | "secondary"
  | "section"
  | "meta";

/* ── Text ─────────────────────────────────────────────────────────── */
// Themed Text. `variant` picks a step of the type scale (default body);
// `style` overrides it.
export function Txt({
  children,
  style,
  numberOfLines,
  variant = "body",
  testID,
  accessibilityRole,
  accessibilityLiveRegion,
}: {
  children: React.ReactNode;
  style?: StyleProp<TextStyle>;
  numberOfLines?: number;
  variant?: TypeKey;
  testID?: string;
  accessibilityRole?: AccessibilityRole;
  // Announce changes (e.g. a result count) — "polite" on Android; iOS reads
  // it when focused.
  accessibilityLiveRegion?: "none" | "polite" | "assertive";
}) {
  const v = ty[variant];
  return (
    <Text
      testID={testID}
      accessibilityRole={accessibilityRole}
      accessibilityLiveRegion={accessibilityLiveRegion}
      numberOfLines={numberOfLines}
      style={[
        {
          fontFamily: v.fontFamily,
          fontSize: v.fontSize,
          lineHeight: v.lineHeight,
          color: v.color,
        },
        style,
      ]}
    >
      {children}
    </Text>
  );
}

/* ── Bottom inset ─────────────────────────────────────────────────── */
// The space bottom chrome (tab bar, BottomAction, sheets) keeps below itself.
// iOS: exactly the home-indicator inset, as before. Android: the gesture /
// navigation-bar inset the root SafeAreaProvider reports (the app is always
// edge-to-edge, so it draws under that bar) — but never less than
// ANDROID_MIN_BOTTOM, because a device or emulator that reports no
// navigation bar (an inset of 0) otherwise leaves labels and CTAs flush with
// the glass edge (review #5).
const ANDROID_MIN_BOTTOM = space.lg;

export function useBottomInset(): number {
  const insets = useSafeAreaInsets();
  if (Platform.OS === "android") {
    return Math.max(insets.bottom, ANDROID_MIN_BOTTOM);
  }
  return insets.bottom;
}

/* ── Screen ───────────────────────────────────────────────────────── */
// Root of every route: background, safe area, keyboard avoidance. Put a
// <Header> (or a tab's large title) as the first child — titles live at the
// TOP of the screen.
export function Screen({
  children,
  testID,
  centered,
  aboveTabBar,
}: {
  children: React.ReactNode;
  // Each route sets `screen-<route>` here so e2e flows have one stable root
  // anchor per screen. Inert in production.
  testID?: string;
  // Single-column screens (auth, self, forms) set this. On `regular` (iPad)
  // width it constrains + centers the content to a readable column; on
  // `compact` (phones, narrow panes) it is a no-op.
  centered?: boolean;
  // Tab screens set this. The tab bar already pads itself by the bottom safe
  // area, so the screen must not pad by it again.
  aboveTabBar?: boolean;
}) {
  // Subscribe to the theme so the whole subtree re-renders (and the token
  // getters resolve to the new colours) when a base colour changes.
  useTheme();
  const cls = useLayoutClass();
  const centerBody = centered && cls === "regular";
  const androidKeyboard = useAndroidKeyboardInset();
  const bottomInset = useBottomInset();
  // The top edge comes from SafeAreaView; the bottom is padded by hand so it
  // gets the same Android floor as the tab bar and sheets (useBottomInset).
  return (
    <SafeAreaView
      testID={testID}
      style={{
        flex: 1,
        backgroundColor: semantic.bg,
        paddingBottom: aboveTabBar ? 0 : bottomInset,
      }}
      edges={["top"]}
    >
      {/* Keeps a bottom <Field>/<BottomAction> above the keyboard.
          iOS: KeyboardAvoidingView. Android: an explicit inset (see
          useAndroidKeyboardInset) — the app is edge-to-edge (#1194) and KAV
          only reacts to keyboard SHOW events. */}
      <KeyboardAvoidingView
        style={{ flex: 1, paddingBottom: androidKeyboard }}
        behavior={Platform.OS === "ios" ? "padding" : undefined}
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
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

/* ── IconButton ───────────────────────────────────────────────────── */
// A 44×44 icon-only control. `accessibilityLabel` is required — the icon is
// silent. `filled` gives it the round raised background (Direct's "New
// message", Groups' "Invite people").
export function IconButton({
  icon,
  onPress,
  accessibilityLabel,
  testID,
  filled,
  disabled,
  style,
}: {
  icon: React.ReactNode;
  onPress?: () => void;
  accessibilityLabel: string;
  testID?: string;
  filled?: boolean;
  disabled?: boolean;
  style?: StyleProp<ViewStyle>;
}) {
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ disabled: !!disabled }}
      style={({ pressed }) => [
        {
          width: layout.touchMin,
          height: layout.touchMin,
          borderRadius: layout.touchMin / 2,
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: filled
            ? semantic.raised
            : pressed
              ? semantic.raised
              : "transparent",
          opacity: disabled ? 0.45 : 1,
        },
        style,
      ]}
    >
      {icon}
    </Pressable>
  );
}

// Header-bar action: an IconButton, named for where it sits.
export const HeaderAction = IconButton;

/* ── Header ───────────────────────────────────────────────────────── */
// Top bar for every screen. `variant="bar"` (default, pushed screens): a
// 44×44 back chevron at the start, title (17/700) + optional subtitle, then
// `actions` (IconButton/HeaderAction, 44×44 each) at the end, over a hairline.
// `variant="large"`: a tab root's 28/700 title with actions and no back.
// Back calls `onBack` or router.back(); the native edge swipe keeps working
// because the stack itself is untouched. The back button keeps the
// `btn-back` testID e2e flows use. Leave `title` out for a back-only bar
// (auth steps, whose large heading sits in the body); pair it with
// `bordered={false}`.
export function Header({
  title,
  subtitle,
  titleIcon,
  actions,
  variant = "bar",
  onBack,
  backTo,
  hideBack,
  bordered = true,
  onTitlePress,
  titleAccessibilityLabel,
  backLabel: backLabelProp,
  backTestID = "btn-back",
  testID,
}: {
  title?: string;
  subtitle?: string;
  // Small glyph before the title (e.g. Icon.hash for a channel).
  titleIcon?: React.ReactNode;
  actions?: React.ReactNode;
  variant?: "bar" | "large";
  onBack?: () => void;
  // Name of the screen back returns to: the label becomes "Back to <x>".
  backTo?: string;
  // Two-pane embedded mode: nothing to pop, so no back button.
  hideBack?: boolean;
  // Hairline under the bar (variant "bar" only).
  bordered?: boolean;
  // Makes the title a button (e.g. a group menu).
  onTitlePress?: () => void;
  titleAccessibilityLabel?: string;
  // Spoken label for the back button when "Back" / "Back to <x>" doesn't
  // say where it goes (e.g. "Use a different email").
  backLabel?: string;
  // Screens whose e2e flows tap a specific back id pass it here.
  backTestID?: string;
  testID?: string;
}) {
  const router = useRouter();
  const { t } = useTranslation(["common", "mobile"]);
  const large = variant === "large";
  const showBack = !large && !hideBack;
  const backLabel =
    backLabelProp ??
    (backTo ? t("mobile:ui.backTo", { name: backTo }) : t("common:actions.back"));

  const titleBlock = title === undefined ? (
    <View style={{ flex: 1 }} />
  ) : (
    <View style={{ flex: 1, minWidth: 0, justifyContent: "center" }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
        {titleIcon}
        <Text
          accessibilityRole="header"
          numberOfLines={1}
          style={[
            large ? ty.display : ty.heading,
            { color: semantic.text, flexShrink: 1 },
          ]}
        >
          {title}
        </Text>
      </View>
      {subtitle ? (
        <Text numberOfLines={1} style={[ty.meta, { fontSize: 13, color: semantic.muted }]}>
          {subtitle}
        </Text>
      ) : null}
    </View>
  );

  return (
    <View
      testID={testID}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: large ? space.sm : 4,
        minHeight: large ? 56 : layout.header,
        paddingStart: large ? space.xxl : 4,
        paddingEnd: large ? space.lg : space.sm,
        paddingTop: large ? 4 : 0,
        paddingBottom: large ? space.lg : space.sm,
        borderBottomWidth: !large && bordered ? 1 : 0,
        borderBottomColor: semantic.hairSoft,
      }}
    >
      {showBack ? (
        <IconButton
          testID={backTestID}
          accessibilityLabel={backLabel}
          onPress={onBack ?? (() => router.back())}
          icon={<Icon.chevronLeft size={24} color={semantic.text} />}
        />
      ) : !large ? (
        <View style={{ width: space.md }} />
      ) : null}
      {onTitlePress ? (
        <Pressable
          onPress={onTitlePress}
          accessibilityRole="button"
          accessibilityLabel={titleAccessibilityLabel ?? title ?? ""}
          style={{ flex: 1, minWidth: 0, minHeight: layout.touchMin, justifyContent: "center" }}
        >
          {titleBlock}
        </Pressable>
      ) : (
        titleBlock
      )}
      {actions ? (
        <View style={{ flexDirection: "row", alignItems: "center", gap: 4 }}>
          {actions}
        </View>
      ) : null}
    </View>
  );
}

/* ── Section title ────────────────────────────────────────────────── */
// 13/600 dim, sentence case, never uppercased or tracked. `right` is an
// optional trailing control (e.g. an IconButton "Create a channel").
// Default padding (20 h) suits a full-bleed list. Inside a padded content
// column pass `paddingHorizontal: 0` so the title shares the column's left
// edge with field labels and body text — never a 4pt nudge (review #11).
export function SectionTitle({
  children,
  right,
  testID,
  style,
}: {
  children: string;
  right?: React.ReactNode;
  testID?: string;
  style?: StyleProp<ViewStyle>;
}) {
  return (
    <View
      style={[
        {
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: space.md,
          paddingHorizontal: space.xxxl,
          paddingTop: space.xxxl,
          paddingBottom: space.sm,
        },
        style,
      ]}
    >
      <Text testID={testID} accessibilityRole="header" style={[ty.section, { flexShrink: 1 }]}>
        {children}
      </Text>
      {right}
    </View>
  );
}

/* ── Dot ──────────────────────────────────────────────────────────── */
// Unread marker (8pt, text colour). Pair it with bold text and, where known,
// a count — state is never colour alone.
export function Dot({ size = 8, color }: { size?: number; color?: string }) {
  return (
    <View
      accessible={false}
      style={{
        width: size,
        height: size,
        borderRadius: size / 2,
        backgroundColor: color ?? semantic.text,
      }}
    />
  );
}

/* ── Divider ──────────────────────────────────────────────────────── */
// 1px hairline. `inset` indents it from the start edge.
export function Divider({ inset = 0, style }: { inset?: number; style?: StyleProp<ViewStyle> }) {
  return (
    <View
      style={[{ height: 1, marginStart: inset, backgroundColor: semantic.hairSoft }, style]}
    />
  );
}

/* ── Chip / Pill ──────────────────────────────────────────────────── */
// Rounded pill, 32pt visual inside a 44pt hit area (the group strip in
// Main.dc.html). Variants: `default` raised + text, `subtle` raised + dim,
// `on` selected (accent tint + accent border + accent text), `solid` accent
// fill, `outline` 1px edge border. `leading`/`trailing` take a Dot, Badge or
// icon.
export function Chip({
  children,
  variant = "default",
  selected,
  onPress,
  style,
  testID,
  accessibilityLabel,
  disabled,
  leading,
  trailing,
}: {
  children: React.ReactNode;
  variant?: "default" | "on" | "solid" | "subtle" | "outline";
  // Same as variant "on"; also announced as selected.
  selected?: boolean;
  onPress?: () => void;
  style?: StyleProp<ViewStyle>;
  testID?: string;
  accessibilityLabel?: string;
  // Press suppressed, state announced, chip dimmed.
  disabled?: boolean;
  leading?: React.ReactNode;
  trailing?: React.ReactNode;
}) {
  const v = selected ? "on" : variant;
  const bg =
    v === "on"
      ? semantic.accentSoft
      : v === "solid"
        ? semantic.accent
        : v === "outline"
          ? "transparent"
          : semantic.raised;
  const border =
    v === "on" ? semantic.accentLine : v === "outline" ? semantic.edge : "transparent";
  const fg =
    v === "on"
      ? semantic.accent
      : v === "solid"
        ? semantic.onAccent
        : v === "subtle"
          ? semantic.dim
          : semantic.text;
  const label = typeof children === "string" ? children : undefined;
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled || !onPress}
      testID={testID}
      accessibilityRole={onPress ? "button" : undefined}
      accessibilityLabel={accessibilityLabel ?? label}
      accessibilityState={{ disabled: !!disabled, selected: v === "on" }}
      style={[
        {
          minHeight: onPress ? layout.touchMin : undefined,
          justifyContent: "center",
          opacity: disabled ? 0.45 : 1,
        },
        style,
      ]}
    >
      <View
        style={{
          minHeight: 32,
          flexDirection: "row",
          alignItems: "center",
          gap: space.xs,
          paddingStart: space.lg,
          paddingEnd: trailing ? space.xs : space.lg,
          borderRadius: 16,
          borderWidth: 1,
          borderColor: border,
          backgroundColor: bg,
        }}
      >
        {leading}
        {label !== undefined ? (
          <Text
            numberOfLines={1}
            style={{
              fontFamily: v === "subtle" ? fonts.medium : fonts.semibold,
              fontSize: 14,
              color: fg,
            }}
          >
            {children}
          </Text>
        ) : (
          children
        )}
        {trailing}
      </View>
    </Pressable>
  );
}

// Alias: the design calls these pills.
export const Pill = Chip;

/* ── Button ───────────────────────────────────────────────────────── */
// Text button, min 44pt (52 when `full`). `primary`: accent fill +
// onAccent text — one per screen. `secondary` (alias `default`): raised fill
// with an edge border. `subtle`: no fill, text colour. `danger` looks like
// secondary — destructive is said by the label (and a confirm step), not a
// third hue. Disabled (every variant): raised fill, DASHED edge border, dim
// label — ≥4.5:1 and told apart from enabled by more than colour.
export function Button({
  children,
  variant = "secondary",
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
  variant?: "primary" | "secondary" | "subtle" | "danger" | "default";
  full?: boolean;
  onPress?: () => void;
  icon?: React.ReactNode;
  iconRight?: React.ReactNode;
  disabled?: boolean;
  // "left" is kept for old call sites; it means the start edge.
  align?: "center" | "start" | "left";
  // `btn-<name>` for e2e flows; accessibilityLabel defaults to the label.
  testID?: string;
  accessibilityLabel?: string;
}) {
  const c = buttonColors(currentTheme(), variant, !!disabled);
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel ?? children}
      accessibilityState={{ disabled: !!disabled }}
      // Disabled is solid colours (theme/button.ts), never a faded button:
      // Android fades each child separately, which sank the label (#3/#4).
      style={({ pressed }) => ({
        opacity: !disabled && pressed ? 0.85 : 1,
        flexDirection: "row",
        alignItems: "center",
        justifyContent: align === "center" ? "center" : "flex-start",
        gap: space.sm,
        minHeight: full ? 52 : layout.touchMin,
        paddingHorizontal: space.xxl,
        paddingVertical: space.sm,
        borderRadius: full ? r.md : layout.touchMin / 2,
        borderWidth: c.border ? 1 : 0,
        borderColor: c.border,
        borderStyle: c.borderStyle,
        backgroundColor: (pressed && !disabled ? c.pressedFill : c.fill) ?? "transparent",
        width: full ? "100%" : undefined,
      })}
    >
      {icon}
      <Text
        style={{
          fontFamily: fonts.semibold,
          fontSize: full ? 16 : 15,
          color: c.label,
        }}
      >
        {children}
      </Text>
      {iconRight}
    </Pressable>
  );
}

/* ── Avatar ───────────────────────────────────────────────────────── */
// Circle with the label's first letter (high fill, dim text); a user icon
// when there is no label. `variant="self"` (alias `amber`) is your own
// avatar: accent tint + accent letter. `solid`: accent fill. `shape="rounded"`
// is the Vault tile (radius 14).
export function Avatar({
  label,
  size = "md",
  variant = "default",
  shape = "circle",
  style,
}: {
  label?: string;
  // sm 30 · md 40 · lg 64, or an explicit size in points.
  size?: "sm" | "md" | "lg" | number;
  variant?: "default" | "self" | "amber" | "solid";
  shape?: "circle" | "rounded";
  style?: StyleProp<ViewStyle>;
}) {
  const dim =
    typeof size === "number" ? size : size === "sm" ? 30 : size === "lg" ? 64 : 40;
  const self = variant === "self" || variant === "amber";
  const solid = variant === "solid";
  const fg = solid ? semantic.onAccent : self ? semantic.accent : semantic.dim;
  const initial = label?.trim()
    ? Array.from(label.trim())[0].toLocaleUpperCase(activeLocale())
    : null;
  return (
    <View
      accessible={false}
      importantForAccessibility="no-hide-descendants"
      style={[
        {
          width: dim,
          height: dim,
          borderRadius: shape === "rounded" ? r.lg : dim / 2,
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: solid
            ? semantic.accent
            : self
              ? semantic.accentMid
              : semantic.high,
        },
        style,
      ]}
    >
      {initial ? (
        <Text
          allowFontScaling={false}
          style={{
            fontFamily: fonts.bold,
            fontSize: Math.round(dim * 0.38),
            color: fg,
          }}
        >
          {initial}
        </Text>
      ) : (
        <Icon.user size={Math.round(dim * 0.5)} color={fg} />
      )}
    </View>
  );
}

/* ── Badge ────────────────────────────────────────────────────────── */
// Count pill: accent fill + onAccent text (`tone="neutral"`: high fill +
// text, for counts that are not unread — e.g. waiting requests). Hidden from
// screen readers: the row's label already says the count.
export function Badge({
  children,
  tone = "accent",
  size = "md",
  testID,
}: {
  children: React.ReactNode;
  tone?: "accent" | "neutral";
  // md 22pt (rows) · sm 18pt (tab bar, pills).
  size?: "sm" | "md";
  testID?: string;
}) {
  const h = size === "sm" ? 18 : 22;
  return (
    <View
      testID={testID}
      accessible={false}
      importantForAccessibility="no-hide-descendants"
      style={{
        minWidth: h,
        height: h,
        paddingHorizontal: size === "sm" ? 4 : space.xs,
        borderRadius: h / 2,
        backgroundColor: tone === "accent" ? semantic.accent : semantic.high,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <Text
        maxFontSizeMultiplier={1.2}
        style={{
          fontFamily: fonts.bold,
          fontSize: size === "sm" ? 11 : 12,
          color: tone === "accent" ? semantic.onAccent : semantic.text,
        }}
      >
        {children}
      </Text>
    </View>
  );
}

/* ── ListRow ──────────────────────────────────────────────────────── */
// One tappable row = one control with one spoken label. Min height 44
// (channels), 52 (settings/actions, the default) or 64 (conversations).
// `glyph`: 20–22pt icon or an Avatar. `name`: 16pt title (bold when
// `unread`, accent when `selected`). `sub`: 14pt dim subtitle. Trailing:
// `value` (14pt muted), `badge` (count), `chevron`, or any `end` node.
// Inside a <Group> rows sit flush; standalone rows get rounded selection.
export function ListRow({
  glyph,
  name,
  sub,
  end,
  value,
  badge,
  chevron,
  unread,
  selected,
  minHeight = 52,
  onPress,
  onLongPress,
  nameStyle,
  testID,
  accessibilityLabel,
  accessibilityHint,
  disabled,
}: {
  glyph?: React.ReactNode;
  name: React.ReactNode;
  sub?: React.ReactNode;
  end?: React.ReactNode;
  value?: string;
  badge?: number;
  chevron?: boolean;
  unread?: boolean;
  selected?: boolean;
  minHeight?: number;
  onPress?: () => void;
  onLongPress?: () => void;
  nameStyle?: StyleProp<TextStyle>;
  // `row-<kind>-<id>` so flows can target a specific record.
  testID?: string;
  // Defaults to "name, sub, value" when those are strings. Pass one
  // explicitly when the row shows state (unread, mentions, time).
  accessibilityLabel?: string;
  accessibilityHint?: string;
  disabled?: boolean;
}) {
  const parts = [name, sub, value].filter(
    (p): p is string => typeof p === "string" && p.length > 0,
  );
  const spoken = accessibilityLabel ?? (parts.length ? parts.join(", ") : undefined);
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      onLongPress={disabled ? undefined : onLongPress}
      disabled={disabled || (!onPress && !onLongPress)}
      testID={testID}
      accessible
      accessibilityRole={onPress ? "button" : undefined}
      accessibilityLabel={spoken}
      accessibilityHint={accessibilityHint}
      accessibilityState={{ selected: !!selected, disabled: !!disabled }}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.xl,
        minHeight: Math.max(layout.touchMin, minHeight),
        paddingVertical: space.sm,
        paddingStart: space.xxl,
        paddingEnd: space.xl,
        borderRadius: selected ? r.sm : 0,
        backgroundColor: selected
          ? semantic.accentSoft
          : pressed && onPress
            ? semantic.high
            : "transparent",
        opacity: disabled ? 0.45 : 1,
      })}
    >
      {glyph !== undefined && (
        <View style={{ minWidth: 22, alignItems: "center" }}>{glyph}</View>
      )}
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        {typeof name === "string" ? (
          <Text
            numberOfLines={2}
            style={[
              {
                fontFamily: unread || selected ? fonts.bold : fonts.medium,
                fontSize: 16,
                color: selected ? semantic.accent : semantic.text,
              },
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
            <Text numberOfLines={1} style={ty.secondary}>
              {sub}
            </Text>
          ) : (
            sub
          ))}
      </View>
      {value !== undefined ? (
        <Text numberOfLines={1} style={[ty.secondary, { color: semantic.muted }]}>
          {value}
        </Text>
      ) : null}
      {badge !== undefined && badge > 0 ? <Badge>{badge > 99 ? "99+" : badge}</Badge> : null}
      {end !== undefined && (
        <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
          {end}
        </View>
      )}
      {chevron ? <Icon.chevronRight size={18} color={semantic.muted} /> : null}
    </Pressable>
  );
}

/* ── ActionRow ────────────────────────────────────────────────────── */
// A grouped-list row that carries its own button (Unblock, Revoke…). Unlike
// ListRow the row itself is not one accessible element — that would swallow
// the button — so the text block is one element with the full spoken label
// and `action` is a second, separately focusable control.
export function ActionRow({
  glyph,
  name,
  sub,
  action,
  testID,
  accessibilityLabel,
  minHeight = 64,
}: {
  glyph?: React.ReactNode;
  name: string;
  sub?: string;
  action?: React.ReactNode;
  testID?: string;
  accessibilityLabel?: string;
  minHeight?: number;
}) {
  return (
    <View
      testID={testID}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.xl,
        minHeight: Math.max(layout.touchMin, minHeight),
        paddingVertical: space.sm,
        paddingStart: space.xxl,
        paddingEnd: space.lg,
      }}
    >
      {glyph !== undefined ? (
        <View style={{ minWidth: 22, alignItems: "center" }}>{glyph}</View>
      ) : null}
      <View
        accessible
        accessibilityLabel={accessibilityLabel ?? (sub ? `${name}, ${sub}` : name)}
        style={{ flex: 1, minWidth: 0, gap: 2 }}
      >
        <Text numberOfLines={2} style={{ fontFamily: fonts.medium, fontSize: 16, color: semantic.text }}>
          {name}
        </Text>
        {sub ? <Text style={ty.secondary}>{sub}</Text> : null}
      </View>
      {action}
    </View>
  );
}

/* ── Group ────────────────────────────────────────────────────────── */
// Grouped list (You.dc.html): radius 14, raised fill, rows separated by
// hairSoft lines. Pass ListRows as children; `title` adds a SectionTitle
// above. `surface="high"` for groups inside a sheet (Actions.dc.html).
export function Group({
  children,
  title,
  surface = "raised",
  style,
  testID,
}: {
  children: React.ReactNode;
  title?: string;
  surface?: "raised" | "high" | "panel";
  style?: StyleProp<ViewStyle>;
  testID?: string;
}) {
  const rows = React.Children.toArray(children).filter(React.isValidElement);
  const box = (
    <View
      testID={testID}
      style={[
        {
          borderRadius: r.lg,
          overflow: "hidden",
          backgroundColor: semantic[surface],
        },
        title ? null : style,
      ]}
    >
      {rows.map((row, i) => (
        <React.Fragment key={row.key ?? i}>
          {i > 0 ? <View style={{ height: 1, backgroundColor: semantic.hairSoft }} /> : null}
          {row}
        </React.Fragment>
      ))}
    </View>
  );
  if (!title) {
    return box;
  }
  return (
    <View style={[{ gap: space.sm }, style]}>
      {/* Flush with the group's edge, so a column of field labels, body
          text and group titles shares one left edge (review #11). */}
      <SectionTitle style={{ paddingHorizontal: 0, paddingTop: 0, paddingBottom: 0 }}>
        {title}
      </SectionTitle>
      {box}
    </View>
  );
}

/* ── Card ─────────────────────────────────────────────────────────── */
// Plain rounded container (radius 14, raised fill, 16pt padding) for
// free-form content. Lists of rows use <Group> instead.
export function Card({
  children,
  style,
  surface = "raised",
  testID,
}: {
  children: React.ReactNode;
  style?: StyleProp<ViewStyle>;
  surface?: "raised" | "high" | "panel";
  testID?: string;
}) {
  return (
    <View
      testID={testID}
      style={[
        {
          backgroundColor: semantic[surface],
          padding: space.xxl,
          borderRadius: r.lg,
        },
        style,
      ]}
    >
      {children}
    </View>
  );
}

/* ── Field ────────────────────────────────────────────────────────── */
// Text input: raised fill, 1px edge border (accent while focused or when
// `amber`), radius 12, min 44pt, 16pt text, accent cursor and selection.
// Accepts every TextInput prop; `icon` / `trailing` sit inside the box.
// `ref` reaches the TextInput (React 19 passes it as a plain prop).
export function Field({
  ref,
  icon,
  trailing,
  amber,
  testID,
  accessibilityLabel,
  containerStyle,
  style,
  onFocus,
  onBlur,
  autoCapitalize = "none",
  ...rest
}: Omit<TextInputProps, "selectionColor" | "cursorColor" | "placeholderTextColor"> & {
  ref?: React.Ref<TextInput>;
  icon?: React.ReactNode;
  trailing?: React.ReactNode;
  // Highlighted (e.g. an active verification code box).
  amber?: boolean;
  // `input-<name>` on the TextInput; accessibilityLabel comes from the
  // caller's visible label (Field renders none of its own).
  testID?: string;
  accessibilityLabel?: string;
  containerStyle?: StyleProp<ViewStyle>;
}) {
  const [focused, setFocused] = useState(false);
  return (
    <View
      style={[
        {
          flexDirection: "row",
          alignItems: rest.multiline ? "flex-start" : "center",
          gap: space.sm,
          minHeight: layout.touchMin,
          borderWidth: 1,
          borderColor: amber || focused ? semantic.accent : semantic.edge,
          backgroundColor: semantic.raised,
          paddingVertical: rest.multiline ? space.md : 0,
          paddingHorizontal: space.lg,
          borderRadius: r.md,
        },
        containerStyle,
      ]}
    >
      {icon}
      <TextInput
        ref={ref}
        testID={testID}
        accessibilityLabel={accessibilityLabel}
        placeholderTextColor={semantic.muted}
        selectionColor={semantic.accent}
        cursorColor={semantic.accent}
        autoCapitalize={autoCapitalize}
        onFocus={(e) => {
          setFocused(true);
          onFocus?.(e);
        }}
        onBlur={(e) => {
          setFocused(false);
          onBlur?.(e);
        }}
        {...rest}
        style={[
          {
            flex: 1,
            minHeight: layout.touchMin - 2,
            fontFamily: fonts.regular,
            fontSize: 16,
            color: semantic.text,
            paddingVertical: 0,
            paddingHorizontal: 0,
          },
          style,
        ]}
      />
      {trailing}
    </View>
  );
}

/* ── Toggle ───────────────────────────────────────────────────────── */
// On/off switch: 44×26 track (accent when on, high + edge border when off)
// inside a 44pt hit area. Exposes switch semantics with the checked state.
export function Toggle({
  on,
  onPress,
  testID,
  accessibilityLabel,
  disabled,
}: {
  on?: boolean;
  onPress?: () => void;
  // `toggle-<name>`.
  testID?: string;
  accessibilityLabel?: string;
  disabled?: boolean;
}) {
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled}
      testID={testID}
      accessibilityRole="switch"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ checked: !!on, disabled: !!disabled }}
      hitSlop={{ top: 9, bottom: 9 }}
      style={{
        width: 44,
        height: 26,
        borderRadius: 13,
        borderWidth: 1,
        borderColor: on ? semantic.accent : semantic.edge,
        backgroundColor: on ? semantic.accent : semantic.high,
        justifyContent: "center",
        alignItems: on ? "flex-end" : "flex-start",
        paddingHorizontal: 2,
        opacity: disabled ? 0.45 : 1,
      }}
    >
      <View
        style={{
          width: 20,
          height: 20,
          borderRadius: 10,
          backgroundColor: on ? semantic.onAccent : semantic.dim,
        }}
      />
    </Pressable>
  );
}

/* ── Bottom action zone ───────────────────────────────────────────── */
// Pinned footer for a screen's primary action(s), above the keyboard.
export function BottomAction({ children }: { children: React.ReactNode }) {
  return (
    <View
      style={{
        gap: space.md,
        paddingVertical: space.xl,
        paddingHorizontal: space.xxl,
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
        backgroundColor: semantic.bg,
      }}
    >
      {children}
    </View>
  );
}

/* ── Scrollable body ──────────────────────────────────────────────── */
// The scrolling content area under a Header. Taps reach fields/buttons
// while the keyboard is up.
export function Body({
  children,
  contentContainerStyle,
  testID,
}: {
  children: React.ReactNode;
  contentContainerStyle?: StyleProp<ViewStyle>;
  testID?: string;
}) {
  return (
    <ScrollView
      testID={testID}
      style={{ flex: 1 }}
      contentContainerStyle={[{ paddingBottom: space.xxxl }, contentContainerStyle]}
      keyboardShouldPersistTaps="handled"
    >
      {children}
    </ScrollView>
  );
}
