import { useState } from "react";
import { View, Text, type LayoutChangeEvent } from "react-native";
import { useTranslation } from "react-i18next";
import { useObserver } from "mobx-react-lite";
import { Chip, SectionTitle } from "./ui";
import { Icon } from "./icons";
import { type as ty, semantic, space, fonts } from "../theme/tokens";
import { SUPPORTED_LANGUAGES } from "../i18n/languages";
import { layoutRestartPending, setLanguage } from "../i18n";
import { appStore } from "../stores/appStore";

/**
 * Language picker for the Preferences screen — the mobile counterpart of
 * desktop's `Preferences/LanguageSection`.
 *
 * A row of chips, each labelled with the language's own name for itself: the
 * person reaching for this control is the one who cannot read the English
 * name. The choice is stored on the device (scoped to the signed-in user),
 * never in the synced blob.
 *
 * Switching between an LTR and an RTL language flips the layout only on the
 * next launch (`I18nManager` is applied at startup), so the section says so
 * rather than leaving a mirrored chip row under an unmirrored screen.
 */
// Geist covers Latin and Cyrillic only. A label in another script (Arabic,
// Chinese) set in Geist falls back to the platform font at its regular weight
// on Android — dimmer and smaller than its neighbours — so those labels use the
// system font at the same size, weight and colour instead.
const GEIST_SCRIPTS = /^[\u0000-\u024F\u0400-\u04FF\s]*$/;

function ChipLabel({ label, selected }: { label: string; selected: boolean }) {
  const geist = GEIST_SCRIPTS.test(label);
  return (
    <Text
      numberOfLines={1}
      style={[
        { fontSize: 14, color: selected ? semantic.accent : semantic.text },
        geist ? { fontFamily: fonts.semibold } : { fontWeight: "600" },
      ]}
    >
      {label}
    </Text>
  );
}

export function LanguageSection({ onLayout }: { onLayout?: (e: LayoutChangeEvent) => void }) {
  const { t, i18n } = useTranslation("settings");
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const [restartPending, setRestartPending] = useState(layoutRestartPending);
  const active = i18n.language;

  return (
    <View testID="pref-language" onLayout={onLayout} style={{ gap: space.sm }}>
      <SectionTitle
        testID="pref-language-heading"
        style={{ paddingHorizontal: 4, paddingTop: 0, paddingBottom: 0 }}
      >
        {t("language.heading")}
      </SectionTitle>
      <View
        style={{ flexDirection: "row", flexWrap: "wrap", gap: space.sm }}
        accessibilityRole="radiogroup"
        accessibilityLabel={t("language.ariaLabel")}
      >
        {SUPPORTED_LANGUAGES.map((option) => {
          const selected = active === option.code;
          return (
            <Chip
              key={option.code}
              testID={`chip-language-${option.code}`}
              accessibilityLabel={option.label}
              selected={selected}
              leading={
                selected ? <Icon.check size={16} color={semantic.accent} /> : undefined
              }
              onPress={() => {
                if (selected) {
                  return;
                }
                void setLanguage(option.code, userId).then(() => {
                  setRestartPending(layoutRestartPending());
                });
              }}
            >
              <ChipLabel label={option.label} selected={selected} />
            </Chip>
          );
        })}
      </View>
      <Text style={[ty.meta, { paddingHorizontal: 4 }]}>{t("language.description")}</Text>
      {restartPending ? (
        <View
          style={{ flexDirection: "row", alignItems: "flex-start", gap: space.xs, paddingHorizontal: 4 }}
        >
          <View style={{ paddingTop: 1 }}>
            <Icon.info size={16} color={semantic.accent} />
          </View>
          <Text
            testID="pref-language-restart"
            accessibilityRole="alert"
            style={[ty.secondary, { flex: 1, color: semantic.text }]}
          >
            {t("mobile:language.restartRequired")}
          </Text>
        </View>
      ) : null}
    </View>
  );
}
