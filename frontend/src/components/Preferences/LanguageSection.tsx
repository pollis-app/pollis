import React from "react";
import { useTranslation } from "react-i18next";
import { RadioList } from "../ui/RadioList";
import { supportedLanguages } from "../../i18n/languages";
import { setLanguage } from "../../i18n";

interface LanguageSectionProps {
  /** Scopes the stored choice per user. */
  userId: string | null;
}

/** Language picker; each option uses the language's own name. */
export const LanguageSection: React.FC<LanguageSectionProps> = ({ userId }) => {
  const { t, i18n } = useTranslation("settings");
  const active = i18n.language;

  return (
    <section className="flex flex-col gap-4 mb-12" data-testid="pref-language">
      <h2
        data-testid="pref-language-heading"
        className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b border-line text-fg"
      >
        {t("language.heading")}
      </h2>
      {/* A pick-one setting, so a vertical radio list (#1725): it grows with every locale. */}
      <RadioList
        aria-label={t("language.ariaLabel")}
        data-testid="pref-language-list"
        optionTestId={(option) => `pref-language-${option.value}`}
        options={supportedLanguages().map((option) => ({ value: option.code, label: option.label, lang: option.code }))}
        value={active}
        onChange={(code) => {
          if (code !== active) {
            void setLanguage(code, userId);
          }
        }}
      />
      <p className="text-xs font-mono text-muted">
        {t("language.description")}
      </p>
    </section>
  );
};
