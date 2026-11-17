import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { NavRow } from "../ui/NavRow";
import { languageOption } from "../../i18n/languages";

/** Preferences' Language row: the current language by its own name, opening the Language page (#1944). */
export const LanguageSection: React.FC = () => {
  const { t, i18n } = useTranslation("settings");
  const navigate = useNavigate();
  const active = languageOption(i18n.language);

  return (
    <section className="flex flex-col gap-4 mb-12" data-testid="pref-language">
      <h2
        id="pref-language-heading"
        data-testid="pref-language-heading"
        className="text-secondary font-mono font-medium uppercase tracking-widest pb-1 border-b border-line text-fg"
      >
        {t("language.heading")}
      </h2>
      {/* The list outgrew Preferences (#1944): it has its own page, reached from this row. */}
      <NavRow
        data-testid="pref-language-row"
        aria-labelledby="pref-language-heading pref-language-current"
        textId="pref-language-current"
        lang={active?.code}
        onClick={() => void navigate({ to: "/preferences/language" })}
      >
        {active?.label ?? i18n.language}
      </NavRow>
    </section>
  );
};
