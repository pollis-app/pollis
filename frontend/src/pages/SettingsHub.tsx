import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { Palette, User, ShieldCheck } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";

export const SettingsHubPage: React.FC = () => {
  const navigate = useNavigate();

  const items: TerminalMenuItem[] = [
    {
      id: "preferences",
      label: "Preferences",
      icon: <Palette size={14} />,
      description: "Colors, font size, etc.",
      action: () => navigate({ to: "/preferences" }),
      testId: "menu-item-preferences",
    },
    {
      id: "user",
      label: "User",
      icon: <User size={14} />,
      description: "Profile, username, avatar",
      action: () => navigate({ to: "/user" }),
      testId: "menu-item-user",
    },
    {
      id: "security",
      label: "Security",
      icon: <ShieldCheck size={14} />,
      description: "Device enrollments, identity resets",
      action: () => navigate({ to: "/security" }),
      testId: "menu-item-security",
    },
  ];

  return (
    <TerminalMenu
      items={items}
      onEsc={() => navigate({ to: "/" })}
    />
  );
};
