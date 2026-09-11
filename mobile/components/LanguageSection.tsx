import { useState } from "react";
import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import { useObserver } from "mobx-react-lite";
import { Chip } from "./ui";
import { type as ty, semantic } from "../theme/tokens";
import { SUPPORTED_LANGUAGES } from "../i18n/languages";
import { layoutRestartPending, setLanguage, upper } from "../i18n";
import { appStore } from "../stores/appStore";

/**
 * Language picker for the Preferences screen — the mobile counterpart of
 * desktop's `Preferences/LanguageSection`.
 *
 * A row of chips, like the theme and density pickers beside it, each labelled
 * with the language's own name for itself: the person reaching for this
 * control is the one who cannot read the English name. The choice is stored
 * on the device (scoped to the signed-in user), never in the synced blob.
 *
 * Switching between an LTR and an RTL language flips the layout only on the
 * next launch (`I18nManager` is applied at startup), so the section says so
 * rather than leaving a mirrored chip row under an unmirrored screen.
 */
export function LanguageSection() {
  const { t, i18n } = useTranslation("settings");
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const [restartPending, setRestartPending] = useState(layoutRestartPending);
  const active = i18n.language;

  return (
    <View style={{ paddingHorizontal: 18, paddingTop: 18 }} testID="pref-language">
      <Text style={[ty.label, { marginBottom: 10 }]} testID="pref-language-heading">
        {upper(t("language.heading"))}
      </Text>
      <View
        style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}
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
              variant={selected ? "on" : "default"}
              onPress={() => {
                if (selected) {
                  return;
                }
                void setLanguage(option.code, userId).then(() => {
                  setRestartPending(layoutRestartPending());
                });
              }}
            >
              {option.label}
            </Chip>
          );
        })}
      </View>
      <Text style={[ty.body, { color: semantic.mute, fontSize: 12, marginTop: 10 }]}>
        {t("language.description")}
      </Text>
      {restartPending ? (
        <Text
          testID="pref-language-restart"
          style={[ty.body, { color: semantic.accent, fontSize: 12, marginTop: 6 }]}
        >
          {t("mobile:language.restartRequired")}
        </Text>
      ) : null}
    </View>
  );
}
