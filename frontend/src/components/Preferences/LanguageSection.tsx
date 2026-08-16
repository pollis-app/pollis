import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { supportedLanguages } from "../../i18n/languages";
import { setLanguage } from "../../i18n";

interface LanguageSectionProps {
  /**
   * The signed-in user, or null. Scopes the stored choice so two Pollis users
   * sharing one OS account keep separate languages.
   */
  userId: string | null;
}

/**
 * Language picker.
 *
 * A radiogroup of `Button`s rather than a dropdown: dropdowns are overlays and
 * this repo forbids them outright, and the same shape is already the house
 * pattern for the skin and retention pickers — so it needs no per-skin
 * treatment to look right in `terminal` and `refined` alike.
 *
 * Each option is labelled with the language's own name for itself, not its
 * English name, because the person who needs this control is by definition the
 * person who cannot read the English one.
 */
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
      <div
        role="radiogroup"
        aria-label={t("language.ariaLabel")}
        className="flex gap-2 flex-wrap"
      >
        {supportedLanguages().map((option) => {
          const selected = active === option.code;
          return (
            <Button
              key={option.code}
              variant={selected ? "primary" : "secondary"}
              size="sm"
              aria-pressed={selected}
              aria-label={option.label}
              data-testid={`pref-language-${option.code}`}
              onClick={() => {
                if (selected) {
                  return;
                }
                void setLanguage(option.code, userId);
              }}
            >
              {option.label}
            </Button>
          );
        })}
      </div>
      <p className="text-xs font-mono text-muted">
        {t("language.description")}
      </p>
    </section>
  );
};
