import { useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body } from "../../components/ui";
import { LanguagePicker } from "../../components/LanguagePicker";
import { useThemeRerender } from "../../components/theme";
import { space } from "../../theme/tokens";

// #1944: the language list, its own page so Preferences stays short. Reached from Preferences' Language
// row and from the Self hub's (`from=self`, so Back names where it goes).
export default function Language() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const { from } = useLocalSearchParams<{ from?: string }>();

  return (
    <Screen testID="screen-self-language">
      <Header
        title={t("language.heading")}
        titleTestID="pref-language-heading"
        backTo={from === "self" ? t("mobile:self.title") : t("preferences.title")}
      />
      <Body
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
        }}
      >
        <LanguagePicker />
      </Body>
    </Screen>
  );
}
