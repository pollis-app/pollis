import React from "react";
import { Button } from "./ui/Button";

export type ManagedInstallInfo = {
  kind: "aur";
  display_name: string;
  update_command: string;
};

type Props = {
  info: ManagedInstallInfo;
};

/**
 * Hard-stop screen rendered when an update is available AND the running
 * install is owned by a system package manager that the in-app updater
 * can't replace. The user must update via the package manager (currently
 * AUR) before the rest of the app will render.
 *
 * Backend detection lives in `commands/install_kind.rs::detect`. Add new
 * kinds there (Mac App Store, Microsoft Store, snap, flatpak) and they'll
 * surface here automatically — only the wording is platform-specific.
 */
export const ManagedInstallScreen: React.FC<Props> = ({ info }) => {
  const onCopy = async () => {
    try {
      await navigator.clipboard.writeText(info.update_command);
    } catch {
      // clipboard may be unavailable; ignore
    }
  };

  return (
    <div
      data-testid="managed-install-screen"
      style={{
        height: "100%",
        width: "100%",
        background: "var(--c-bg)",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        fontFamily: "var(--font-mono, monospace)",
      }}
    >
      <div
        style={{
          width: "100%",
          maxWidth: 520,
          padding: "2rem",
          display: "flex",
          flexDirection: "column",
          gap: "1.25rem",
        }}
      >
        <div className="text-sm font-mono" style={{ color: "var(--c-text)" }}>
          Update required
        </div>
        <div className="text-xs font-mono" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
          A new version of Pollis is available. This install is managed by{" "}
          {info.display_name} — the in-app updater can't replace package-manager
          installs. Update from a terminal, then relaunch Pollis.
        </div>
        <div
          style={{
            background: "var(--c-bg-elevated, var(--c-bg))",
            border: "1px solid var(--c-border)",
            padding: "0.75rem 1rem",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: "0.75rem",
          }}
        >
          <code className="text-xs font-mono" style={{ color: "var(--c-text)" }}>
            {info.update_command}
          </code>
          <Button data-testid="managed-install-copy" size="sm" onClick={onCopy}>
            Copy
          </Button>
        </div>
      </div>
    </div>
  );
};
