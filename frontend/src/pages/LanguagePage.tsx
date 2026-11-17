import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { PageShell } from "../components/Layout/PageShell";
import { RadioList } from "../components/ui/RadioList";
import { supportedLanguages } from "../i18n/languages";
import { setLanguage } from "../i18n";
import { appStore } from "../stores/appStore";

/** The language picker (#1944), reached from Preferences; a pick applies at once. */
export const LanguagePage: React.FC = observer(() => {
  const { t, i18n } = useTranslation("settings");
  const userId = appStore.currentUser?.id ?? null;
  const active = i18n.language;

  return (
    <PageShell title={t("language.heading")} scrollable>
      <div data-testid="language-page" className="flex justify-center px-6 py-8">
        <section className="w-full max-w-md flex flex-col gap-4">
          <h2
            data-testid="language-page-heading"
            className="text-secondary font-mono font-medium uppercase tracking-widest pb-1 border-b border-line text-fg"
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
          <p data-testid="language-page-note" className="text-secondary font-mono text-muted">
            {t("language.description")}
          </p>
        </section>
      </div>
    </PageShell>
  );
});
