import React from "react";
import {
  ActivityIndicator,
  View,
  Text,
  TextInput,
  Pressable,
  ScrollView,
  Platform,
  StyleProp,
  ViewStyle,
  TextStyle,
  TextInputProps,
  AccessibilityRole,
  useWindowDimensions,
} from "react-native";
import { SafeAreaView, useSafeAreaInsets } from "react-native-safe-area-context";
import { KeyboardAvoidingView } from "react-native-keyboard-controller";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { semantic, type as ty, fonts, r, space, layout, currentTheme } from "../theme/tokens";
import { buttonColors, checkboxColors, controlDisabled, type ControlSurface } from "../theme/button";
import { useTheme } from "./theme";
import { ThemeProbe } from "./ThemeProbe";
import { EmojiProbe } from "./emoji/EmojiProbe";
import { useLayoutClass } from "../hooks/useLayoutClass";
import { PaneEntryContext } from "./pane/paneContext";
import { activeLocale } from "../i18n";
import { Icon, isIconElement } from "./icons";
import { haptic } from "../lib/haptics";
import { createPressInGuard, pressInAllowed, pressInSupported } from "../lib/pressIn";

// Every primitive: 44pt minimum touch targets, one spoken label per control,
// start/end edges (never left/right), system font scaling kept.

type TypeKey =
  | "display"
  | "title"
  | "heading"
  | "body"
  | "secondary"
  | "section"
  | "meta";

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
  // Android only; iOS reads the change when focused.
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

// Bottom chrome's inset. Android's is floored at ANDROID_MIN_BOTTOM: a device
// that reports no navigation bar (0) leaves CTAs flush with the glass edge.
const ANDROID_MIN_BOTTOM = space.lg;

export function useBottomInset(): number {
  const insets = useSafeAreaInsets();
  if (Platform.OS === "android") {
    return Math.max(insets.bottom, ANDROID_MIN_BOTTOM);
  }
  return insets.bottom;
}

const ScreenBleedContext = React.createContext(0);

// True under a <Screen>. A nested <Screen> (an iPad pane page) must not avoid
// the keyboard a second time.
const KeyboardAvoidedContext = React.createContext(false);

/** The bleed (pt) on each side of the current <Screen>'s centred column; see <BleedHairline>. */
export function useScreenBleed(): number {
  return React.useContext(ScreenBleedContext);
}

/**
 * A 1px hairline across the whole screen, even inside a <Screen>'s centred
 * column. The parent must not clip (`overflow: "hidden"`).
 */
export function BleedHairline({ edge }: { edge: "top" | "bottom" }) {
  const bleed = useScreenBleed();
  return (
    <View
      accessible={false}
      pointerEvents="none"
      style={{
        position: "absolute",
        start: -bleed,
        end: -bleed,
        height: 1,
        ...(edge === "top" ? { top: 0 } : { bottom: 0 }),
        backgroundColor: semantic.hairSoft,
      }}
    />
  );
}

// Root of every route: background, safe area, keyboard avoidance; a <Header>
// goes first. On `regular` width (iPad) content sits in a centred
// `layout.screenMaxWidth` column; `wide` opts out, `centered` (auth steps)
// narrows it to `layout.authMaxWidth`.
export function Screen({
  children,
  testID,
  centered,
  wide,
  aboveTabBar,
}: {
  children: React.ReactNode;
  // `screen-<route>`: the e2e root anchor.
  testID?: string;
  // Ignored inside the iPad detail pane, whose pages all share one width.
  centered?: boolean;
  wide?: boolean;
  // The tab bar already pads itself by the bottom safe area.
  aboveTabBar?: boolean;
}) {
  // Re-renders the screen's own surfaces, not `children`, so every route
  // subscribes itself (#1447).
  useTheme();
  const cls = useLayoutClass();
  const { width: windowWidth } = useWindowDimensions();
  // A detail pane is narrower than the window; assume the window until measured.
  const [measured, setMeasured] = React.useState<number | null>(null);
  const width = measured ?? windowWidth;
  const inPane = React.useContext(PaneEntryContext) !== null;
  const maxWidth = centered && !inPane ? layout.authMaxWidth : layout.screenMaxWidth;
  const column = !wide && cls === "regular";
  const bleed = column ? Math.max(0, (width - maxWidth) / 2) : 0;
  const nested = React.useContext(KeyboardAvoidedContext);
  const bottomInset = useBottomInset();
  // The bottom is padded by hand to get useBottomInset's Android floor.
  return (
    <SafeAreaView
      testID={testID}
      onLayout={(e) => setMeasured(e.nativeEvent.layout.width)}
      style={{
        flex: 1,
        backgroundColor: semantic.bg,
        paddingBottom: aboveTabBar ? 0 : bottomInset,
      }}
      edges={["top"]}
    >
      <ThemeProbe />
      <EmojiProbe />
      {/* No `automaticOffset` (#1432): its async window position can be stale on
          Android. The layout frame is exact, since this view starts at the window's top. */}
      <KeyboardAvoidingView
        style={{ flex: 1 }}
        behavior="padding"
        enabled={!nested}
      >
        <KeyboardAvoidedContext.Provider value={true}>
          <ScreenBleedContext.Provider value={bleed}>
            {column ? (
              <View
                style={{
                  flex: 1,
                  width: "100%",
                  maxWidth,
                  alignSelf: "center",
                }}
              >
                {children}
              </View>
            ) : (
              children
            )}
          </ScreenBleedContext.Provider>
        </KeyboardAvoidedContext.Provider>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

function isTintableGlyph(node: React.ReactNode): node is React.ReactElement<{ color?: string }> {
  return React.isValidElement<{ color?: string }>(node) && isIconElement(node);
}

function disabledGlyph(node: React.ReactNode, disabled: boolean | undefined): React.ReactNode {
  if (!disabled || !isTintableGlyph(node)) {
    return node;
  }
  return React.cloneElement(node, { color: controlDisabled(currentTheme()).fg });
}

function tintGlyph(node: React.ReactNode, color: string): React.ReactNode {
  if (!isTintableGlyph(node)) {
    return node;
  }
  return React.cloneElement(node, { color });
}

// True inside a SheetOverlay (a Modal). See pressInSupported.
export const InSheetContext = React.createContext(false);

// Opt-in touch-down activation (#1529; rules in lib/pressIn.ts), spread onto a
// Pressable. onPressIn acts for a finger, onPress for TalkBack's click (no
// touch-down); the guard runs the action once per touch. Never on a row in a
// ScrollView / FlatList, or with a long press.
export function usePressIn(
  onPress: (() => void) | undefined,
  enabled: boolean | undefined,
): { onPress?: () => void; onPressIn?: () => void; onPressOut?: () => void } {
  const guard = React.useRef<ReturnType<typeof createPressInGuard> | null>(null);
  const latest = React.useRef(onPress);
  latest.current = onPress;
  const inSheet = React.useContext(InSheetContext);
  if (!enabled || !onPress || !pressInSupported(Platform.OS, inSheet)) {
    return { onPress };
  }
  if (!guard.current) {
    guard.current = createPressInGuard();
  }
  const g = guard.current;
  const run = () => latest.current?.();
  return {
    onPressIn: () => g.onPressIn(run),
    onPress: () => g.onPress(run),
    onPressOut: () => g.onPressOut(),
  };
}

// A bare Pressable with the opt-in `pressIn` (#1529).
export function PressInPressable({
  onPress,
  pressIn,
  ...rest
}: Omit<React.ComponentProps<typeof Pressable>, "onPress" | "onPressIn" | "onPressOut" | "onLongPress"> & {
  onPress?: () => void;
  pressIn?: boolean;
}) {
  const press = usePressIn(rest.disabled ? undefined : onPress, pressIn);
  return <Pressable {...rest} {...press} />;
}

// A 44×44 icon-only control; the icon is silent, so `accessibilityLabel` is
// required. The caller's glyph colour is replaced: accent by default, `text`
// for tone="neutral"; "inherit" keeps it. A composed node is never recoloured.
export function IconButton({
  icon,
  onPress,
  accessibilityLabel,
  testID,
  filled,
  disabled,
  tone = "accent",
  style,
  pressIn,
}: {
  icon: React.ReactNode;
  onPress?: () => void;
  // Touch-down (#1529): fixed chrome whose accidental press is free to undo.
  pressIn?: boolean;
  accessibilityLabel: string;
  testID?: string;
  filled?: boolean;
  disabled?: boolean;
  tone?: "accent" | "neutral" | "inherit";
  style?: StyleProp<ViewStyle>;
}) {
  const glyph = disabled
    ? disabledGlyph(icon, true)
    : tone === "inherit"
      ? icon
      : tintGlyph(icon, tone === "neutral" ? semantic.text : semantic.accent);
  const press = usePressIn(disabled ? undefined : onPress, pressIn);
  return (
    <Pressable
      {...press}
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
          // Disabled is a solid fill, never a fade: Android fades children separately.
          backgroundColor: disabled
            ? controlDisabled(currentTheme()).fill
            : filled
              ? pressed
                ? semantic.high
                : semantic.raised
              : pressed
                ? semantic.raised
                : "transparent",
        },
        style,
      ]}
    >
      {glyph}
    </Pressable>
  );
}

export const HeaderAction = IconButton;

// Top bar for every screen: back (`btn-back`), title, `actions`.
// `variant="large"` is a tab root's title, with no back. Leave `title` out for
// a back-only bar and pair it with `bordered={false}`.
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
  titleTestID,
  subtitleTestID,
}: {
  title?: string;
  subtitle?: string;
  titleTestID?: string;
  subtitleTestID?: string;
  titleIcon?: React.ReactNode;
  actions?: React.ReactNode;
  variant?: "bar" | "large";
  onBack?: () => void;
  // The back label becomes "Back to <x>".
  backTo?: string;
  // Two-pane embedded mode: nothing to pop, so no back button.
  hideBack?: boolean;
  // Hairline under the bar (variant "bar" only).
  bordered?: boolean;
  onTitlePress?: () => void;
  titleAccessibilityLabel?: string;
  // When "Back" / "Back to <x>" doesn't say where it goes.
  backLabel?: string;
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
  // The title opens a menu or sheet, free to undo: touch-down (#1529).
  const titlePress = usePressIn(onTitlePress, true);

  const titleBlock = title === undefined ? (
    <View style={{ flex: 1 }} />
  ) : (
    <View style={{ flex: 1, minWidth: 0, justifyContent: "center" }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
        {titleIcon}
        <Text
          testID={titleTestID}
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
        <Text
          testID={subtitleTestID}
          numberOfLines={1}
          style={[ty.meta, { fontSize: 13, color: semantic.muted }]}
        >
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
      }}
    >
      {!large && bordered ? <BleedHairline edge="bottom" /> : null}
      {showBack ? (
        <IconButton
          testID={backTestID}
          accessibilityLabel={backLabel}
          onPress={onBack ?? (() => router.back())}
          pressIn
          tone="neutral"
          icon={<Icon.chevronLeft size={24} color={semantic.text} />}
        />
      ) : !large ? (
        <View style={{ width: space.md }} />
      ) : null}
      {onTitlePress ? (
        <Pressable
          {...titlePress}
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
// Sentence case, never uppercased. Inside a padded column pass
// `paddingHorizontal: 0` so it shares the column's edge with field labels.
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

// Unread marker: pair it with bold text or a count; state is never colour alone.
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

export function Divider({ inset = 0, style }: { inset?: number; style?: StyleProp<ViewStyle> }) {
  return (
    <View
      style={[{ height: 1, marginStart: inset, backgroundColor: semantic.hairSoft }, style]}
    />
  );
}

// A 32pt pill inside a ≥44×44 hit area; shape comes from the fill, never a
// border. `outline` is for a pill on a raised row or card.
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
  pressIn,
}: {
  children: React.ReactNode;
  variant?: "default" | "on" | "solid" | "subtle" | "outline";
  // Touch-down (#1529): fixed chrome only, never a chip inside a scroller.
  pressIn?: boolean;
  // Same as variant "on"; also announced as selected.
  selected?: boolean;
  onPress?: () => void;
  style?: StyleProp<ViewStyle>;
  testID?: string;
  accessibilityLabel?: string;
  disabled?: boolean;
  leading?: React.ReactNode;
  trailing?: React.ReactNode;
}) {
  const v = selected ? "on" : variant;
  const surface: ControlSurface = variant === "outline" ? "raised" : "base";
  const off = disabled ? controlDisabled(currentTheme(), surface) : null;
  const accentFill = v === "on" || v === "solid";
  const bg = off
    ? off.fill
    : accentFill
      ? semantic.accent
      : v === "subtle"
        ? "transparent"
        : v === "outline"
          ? semantic.high
          : semantic.raised;
  // A disabled chip that is selected keeps an accent label (accent passes
  // 4.5:1 on raised and high), so selection survives the disabled look.
  const fg = off
    ? v === "on"
      ? semantic.accent
      : off.fg
    : accentFill
      ? semantic.onAccent
      : v === "subtle"
        ? semantic.dim
        : semantic.accent;
  const label = typeof children === "string" ? children : undefined;
  const press = usePressIn(disabled ? undefined : onPress, pressIn);
  return (
    <Pressable
      {...press}
      disabled={disabled || !onPress}
      testID={testID}
      accessibilityRole={onPress ? "button" : undefined}
      accessibilityLabel={accessibilityLabel ?? label}
      accessibilityState={{ disabled: !!disabled, selected: v === "on" }}
      style={[
        {
          // ≥44pt in both dimensions around the 32pt pill, so a short label is still
          // a full target.
          minHeight: onPress ? layout.touchMin : undefined,
          minWidth: onPress ? layout.touchMin : undefined,
          justifyContent: "center",
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
          backgroundColor: bg,
        }}
      >
        {disabledGlyph(leading, disabled)}
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

export const Pill = Chip;

// Never a border. `danger` looks like secondary: destructive is said by the
// label and a confirm step, not a third hue. A disabled primary stays a dimmed
// accent so it never reads as secondary. `surface="raised"` on a raised card.
// `loading` disables it and announces busy, so a double tap cannot submit twice.
export function Button({
  children,
  variant = "secondary",
  full,
  onPress,
  icon,
  iconRight,
  disabled,
  loading,
  align = "center",
  testID,
  accessibilityLabel,
  surface = "base",
  pressIn,
}: {
  children: string;
  // Touch-down (#1529): navigation only. Never send, save, confirm or destroy.
  pressIn?: boolean;
  variant?: "primary" | "secondary" | "subtle" | "danger" | "default";
  full?: boolean;
  onPress?: () => void;
  icon?: React.ReactNode;
  iconRight?: React.ReactNode;
  disabled?: boolean;
  loading?: boolean;
  // "left" is kept for old call sites; it means the start edge.
  align?: "center" | "start" | "left";
  testID?: string;
  accessibilityLabel?: string;
  surface?: ControlSurface;
}) {
  const off = !!disabled || !!loading;
  const c = buttonColors(currentTheme(), variant, off, surface);
  const press = usePressIn(off ? undefined : onPress, pressIn);
  return (
    <Pressable
      {...press}
      disabled={off}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel ?? children}
      accessibilityState={{ disabled: off, busy: !!loading }}
      // Solid disabled colours (theme/button.ts): Android fades each child separately.
      style={({ pressed }) => ({
        opacity: !off && pressed ? 0.85 : 1,
        flexDirection: "row",
        alignItems: "center",
        justifyContent: align === "center" ? "center" : "flex-start",
        gap: space.sm,
        minHeight: full ? 52 : layout.touchMin,
        paddingHorizontal: space.xxl,
        paddingVertical: space.sm,
        borderRadius: full ? r.md : layout.touchMin / 2,
        backgroundColor: (pressed && !off ? c.pressedFill : c.fill) ?? "transparent",
        width: full ? "100%" : undefined,
      })}
    >
      {loading ? (
        <ActivityIndicator testID={testID ? `${testID}-busy` : undefined} size="small" color={c.label} />
      ) : (
        tintGlyph(icon, c.label)
      )}
      <Text
        style={{
          fontFamily: fonts.semibold,
          fontSize: full ? 16 : 15,
          color: c.label,
        }}
      >
        {children}
      </Text>
      {loading ? null : tintGlyph(iconRight, c.label)}
    </Pressable>
  );
}

// `variant="self"` (alias `amber`) is your own avatar; `shape="rounded"` is
// the Vault tile.
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

// Hidden from screen readers: the row's label already says the count.
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

// `pressIn` (#1529) only for a row that does not scroll, and never with
// `onLongPress`.
type ListRowPressProps =
  | { pressIn?: false; onLongPress?: () => void }
  | { pressIn: true; onLongPress?: undefined };

// One tappable row = one control with one spoken label.
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
  pressIn,
}: ListRowPressProps & {
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
  nameStyle?: StyleProp<TextStyle>;
  // `row-<kind>-<id>` so flows can target a specific record.
  testID?: string;
  // Pass one when the row shows state (unread, mentions, time).
  accessibilityLabel?: string;
  accessibilityHint?: string;
  disabled?: boolean;
}) {
  const parts = [name, sub, value].filter(
    (p): p is string => typeof p === "string" && p.length > 0,
  );
  const spoken = accessibilityLabel ?? (parts.length ? parts.join(", ") : undefined);
  // The types refuse pressIn with a long press; this holds it at runtime.
  const press = usePressIn(disabled ? undefined : onPress, pressInAllowed(pressIn, !!onLongPress));
  return (
    <Pressable
      {...press}
      onLongPress={
        disabled || !onLongPress
          ? undefined
          : () => {
              haptic("longPress");
              onLongPress();
            }
      }
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
      })}
    >
      {glyph !== undefined && (
        <View style={{ minWidth: 22, alignItems: "center" }}>
          {disabledGlyph(glyph, disabled)}
        </View>
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
              disabled ? { color: semantic.dim } : null,
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
      {/* A disabled row drops its chevron: the non-colour cue beside the dim name. */}
      {chevron && !disabled ? <Icon.chevronRight size={18} color={semantic.muted} /> : null}
    </Pressable>
  );
}

// A row that carries its own button: the text block and `action` are
// separately focusable, since one row element would swallow the button.
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

// Grouped list of ListRows; `surface="high"` for a group inside a sheet.
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
      <SectionTitle style={{ paddingHorizontal: 0, paddingTop: 0, paddingBottom: 0 }}>
        {title}
      </SectionTitle>
      {box}
    </View>
  );
}

// Free-form container; lists of rows use <Group>.
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

// Borderless filled text input; focus shows as the accent cursor, not a ring.
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
  surface = "base",
  autoCapitalize = "none",
  ...rest
}: Omit<TextInputProps, "selectionColor" | "cursorColor" | "placeholderTextColor"> & {
  ref?: React.Ref<TextInput>;
  icon?: React.ReactNode;
  trailing?: React.ReactNode;
  amber?: boolean;
  surface?: ControlSurface;
  // Field renders no label of its own; pass the caller's visible one.
  testID?: string;
  accessibilityLabel?: string;
  containerStyle?: StyleProp<ViewStyle>;
}) {
  return (
    <View
      style={[
        {
          flexDirection: "row",
          alignItems: rest.multiline ? "flex-start" : "center",
          gap: space.sm,
          minHeight: layout.touchMin,
          backgroundColor: amber
            ? semantic.accentSoft
            : surface === "raised"
              ? semantic.high
              : semantic.raised,
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
        {...rest}
        style={[
          {
            flex: 1,
            minHeight: layout.touchMin,
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

// The knob's side says on/off, not colour alone.
export function Toggle({
  on,
  onPress,
  testID,
  accessibilityLabel,
  disabled,
}: {
  on?: boolean;
  onPress?: () => void;
  testID?: string;
  accessibilityLabel?: string;
  disabled?: boolean;
}) {
  return (
    <Pressable
      onPress={
        disabled || !onPress
          ? undefined
          : () => {
              haptic("toggle");
              onPress();
            }
      }
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
        // Toggles sit in raised rows, so the off and disabled tracks are `high`.
        backgroundColor: disabled
          ? controlDisabled(currentTheme(), "raised").fill
          : on
            ? semantic.accent
            : semantic.high,
        justifyContent: "center",
        alignItems: on ? "flex-end" : "flex-start",
        paddingHorizontal: 2,
      }}
    >
      <View
        style={{
          width: 20,
          height: 20,
          borderRadius: 10,
          backgroundColor: disabled ? semantic.dim : on ? semantic.onAccent : semantic.text,
        }}
      />
    </Pressable>
  );
}

// With `onPress` it is its own 44×44 control; without, a silent visual for a
// row that is the control (<CheckRow>).
export function Checkbox({
  checked,
  onPress,
  testID,
  accessibilityLabel,
  disabled,
}: {
  checked: boolean;
  onPress?: () => void;
  testID?: string;
  accessibilityLabel?: string;
  disabled?: boolean;
}) {
  const c = checkboxColors(currentTheme(), checked);
  const box = (
    <View
      accessible={false}
      style={{
        width: 24,
        height: 24,
        borderRadius: 6,
        backgroundColor: c.fill,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {checked ? <Icon.check size={16} color={c.glyph} /> : null}
    </View>
  );
  if (!onPress) {
    return box;
  }
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled}
      testID={testID}
      accessibilityRole="checkbox"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ checked, disabled: !!disabled }}
      style={{
        minWidth: layout.touchMin,
        minHeight: layout.touchMin,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {box}
    </Pressable>
  );
}

export function CheckRow({
  checked,
  onPress,
  label,
  accessibilityLabel,
  testID,
  disabled,
}: {
  checked: boolean;
  onPress: () => void;
  label: string;
  accessibilityLabel?: string;
  testID?: string;
  disabled?: boolean;
}) {
  return (
    <Pressable
      onPress={disabled ? undefined : onPress}
      disabled={disabled}
      testID={testID}
      accessibilityRole="checkbox"
      accessibilityLabel={accessibilityLabel ?? label}
      accessibilityState={{ checked, disabled: !!disabled }}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.xl,
        minHeight: 56,
        paddingHorizontal: space.xxl,
        paddingVertical: space.lg,
        borderRadius: r.md,
        backgroundColor: pressed && !disabled ? semantic.high : semantic.raised,
      })}
    >
      <Checkbox checked={checked} />
      <Text style={[ty.body, { flex: 1 }, disabled ? { color: semantic.dim } : null]}>
        {label}
      </Text>
    </Pressable>
  );
}

// Pinned footer for a screen's primary action(s), above the keyboard.
export function BottomAction({ children }: { children: React.ReactNode }) {
  return (
    <View
      style={{
        gap: space.md,
        paddingVertical: space.xl,
        paddingHorizontal: space.xxl,
        backgroundColor: semantic.bg,
      }}
    >
      <BleedHairline edge="top" />
      {children}
    </View>
  );
}

// Taps reach fields and buttons while the keyboard is up.
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
