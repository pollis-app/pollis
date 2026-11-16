import { useState } from "react";
import { View, Text, type LayoutChangeEvent } from "react-native";
import { useTranslation } from "react-i18next";
import { useObserver } from "mobx-react-lite";
import { Group, SectionTitle } from "./ui";
import { RadioRow } from "./RadioRow";
import { Icon } from "./icons";
import { type as ty, semantic, space } from "../theme/tokens";
import { SUPPORTED_LANGUAGES } from "../i18n/languages";
import { layoutRestartPending, setLanguage } from "../i18n";
import { appStore } from "../stores/appStore";

// A pick-one setting, so a vertical radio list (#1725): it grows with every locale, which a chip row cannot.
// Endonyms outside Geist's Latin/Cyrillic fall back to the platform font for their script.
export function LanguageSection({ onLayout }: { onLayout?: (e: LayoutChangeEvent) => void }) {
  const { t, i18n } = useTranslation("settings");
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const [restartPending, setRestartPending] = useState(layoutRestartPending);
  const active = i18n.language;

  return (
    <View testID="pref-language" onLayout={onLayout} style={{ gap: space.sm }}>
      <SectionTitle
        testID="pref-language-heading"
        style={{ paddingHorizontal: 0, paddingTop: 0, paddingBottom: 0 }}
      >
        {t("language.heading")}
      </SectionTitle>
      <View accessibilityRole="radiogroup" accessibilityLabel={t("language.ariaLabel")}>
        <Group>
          {SUPPORTED_LANGUAGES.map((option) => (
            <RadioRow
              key={option.code}
              testID={`radio-language-${option.code}`}
              label={option.label}
              selected={active === option.code}
              onPress={() => {
                if (active === option.code) {
                  return;
                }
                void setLanguage(option.code, userId).then(() => {
                  setRestartPending(layoutRestartPending());
                });
              }}
            />
          ))}
        </Group>
      </View>
      {/* #1231: "left" is the start edge under RTL. */}
      <Text style={[ty.meta, { textAlign: "left" }]}>{t("language.description")}</Text>
      {restartPending ? (
        <View
          style={{ flexDirection: "row", alignItems: "flex-start", gap: space.xs }}
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
