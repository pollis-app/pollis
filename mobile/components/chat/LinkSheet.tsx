import { useEffect, useRef, useState } from "react";
import { Text, View } from "react-native";
import * as Clipboard from "expo-clipboard";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Group, ListRow } from "../ui";
import { useTheme } from "../theme";
import { SheetOverlay } from "./SheetOverlay";
import { fonts, r, semantic, type as ty } from "../../theme/tokens";
import { haptic } from "../../lib/haptics";
import { openInBrowser } from "../../lib/openLink";
import { appStore } from "../../stores/appStore";
import { linkSheetStore } from "../../stores/linkSheetStore";
import type { LinkTarget } from "../../lib/links";

type CopyState = "idle" | "copied" | "failed";
const COPY_FEEDBACK_MS = 2000;
const GLYPH = 22;

/**
 * The sheet for one message link: its real destination (ASCII host, the bytes the browser
 * gets), Open in browser and Copy link. An IDN host lands here on a plain tap too, with a
 * line saying why. Mounted once at the root; never over the lock screen.
 */
export const LinkSheetHost = observer(function LinkSheetHost() {
  useTheme();
  const target = linkSheetStore.target;
  if (target === null || appStore.isLocked || appStore.currentUser === null) {
    return null;
  }
  return <LinkSheet key={target.href} target={target} confirm={linkSheetStore.mode === "confirm"} />;
});

function LinkSheet({ target, confirm }: { target: LinkTarget; confirm: boolean }) {
  const { t } = useTranslation("mobile");
  const [copy, setCopy] = useState<CopyState>("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) {
        clearTimeout(timer.current);
      }
    },
    [],
  );

  const onCopy = async () => {
    let ok = false;
    try {
      ok = await Clipboard.setStringAsync(target.href);
    } catch {
      ok = false;
    }
    haptic(ok ? "success" : "error");
    setCopy(ok ? "copied" : "failed");
    if (timer.current) {
      clearTimeout(timer.current);
    }
    timer.current = setTimeout(() => setCopy("idle"), COPY_FEEDBACK_MS);
  };

  const onOpen = () => {
    const href = target.href;
    linkSheetStore.close();
    openInBrowser(href);
  };

  const copyColor = copy === "idle" ? semantic.text : semantic.accent;
  const copyLabel =
    copy === "copied" ? t("chat.copied") : copy === "failed" ? t("chat.copyFailed") : t("link.copy");

  return (
    <SheetOverlay
      title={t("link.title")}
      onClose={linkSheetStore.close}
      testID={confirm ? "sheet-link-confirm" : "sheet-link"}
      closeTestID="btn-link-close"
    >
      <View style={{ gap: 12 }}>
        {confirm ? (
          <Text testID="text-link-idn" style={[ty.secondary, { color: semantic.dim }]}>
            {t("link.idnNotice")}
          </Text>
        ) : null}
        {/* The destination: the fill separates it, no border. */}
        <View
          accessible
          accessibilityLabel={t("link.destinationLabel", { host: target.host, url: target.href })}
          style={{
            paddingVertical: 10,
            paddingHorizontal: 12,
            borderRadius: r.md,
            backgroundColor: semantic.panel,
            gap: 4,
          }}
        >
          <Text
            testID="text-link-host"
            style={[ty.body, { fontFamily: fonts.semibold, color: semantic.text, writingDirection: "ltr" }]}
          >
            {target.host}
          </Text>
          <Text
            testID="text-link-href"
            numberOfLines={4}
            style={[ty.mono, { color: semantic.dim, writingDirection: "ltr" }]}
          >
            {target.href}
          </Text>
        </View>
        <Group surface="high">
          <ListRow
            testID="btn-link-open"
            glyph={<Icon.external size={GLYPH} color={semantic.text} />}
            name={t("link.open")}
            onPress={onOpen}
          />
          <ListRow
            testID="btn-link-copy"
            glyph={<Icon.copy size={GLYPH} color={copyColor} />}
            name={copyLabel}
            nameStyle={{ color: copyColor }}
            onPress={() => void onCopy()}
          />
        </Group>
      </View>
    </SheetOverlay>
  );
}
