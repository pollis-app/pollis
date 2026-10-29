import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { supportedLanguages } from "../../i18n/languages";
import { setLanguage } from "../../i18n";

interface LanguageSectionProps {
  /** Scopes the stored choice so users sharing an OS account keep separate languages. */
  userId: string | null;
}

/**
 * Language picker: a radiogroup of `Button`s (no dropdown overlays). Each
 * option is labelled in its own language.
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
