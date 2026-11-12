import React from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeft } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "./TerminalMenu";

interface BackMenuProps {
  items: TerminalMenuItem[];
  /** Run by the trailing "Go back" item and by Esc. */
  onBack: () => void;
}

/** A `TerminalMenu` page that ends in a "Go back" item; Esc goes back too. */
export const BackMenu: React.FC<BackMenuProps> = ({ items, onBack }) => {
  const { t } = useTranslation();

  return (
    <TerminalMenu
      items={[
        ...items,
        {
          id: "__back__",
          activateOn: "press",
          label: t("common:actions.goBack"),
          icon: <ArrowLeft size={14} className="rtl-mirror" />,
          action: onBack,
          type: "system",
        },
      ]}
      onEsc={onBack}
    />
  );
};
