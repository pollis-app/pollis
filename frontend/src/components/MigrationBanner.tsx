import React, { useState } from "react";
import { Download, X } from "lucide-react";
import { hasElectron } from "../bridge/runtime";
import { shellOpen } from "../bridge";
import { Button } from "./ui/Button";

// The marketing site's download section.
const DOWNLOAD_URL = "https://pollis.com/#download";
const DISMISS_KEY = "pollis-electron-eol-dismissed";

/// One-time "this build is end-of-life, download the new one" banner.
///
/// Gated on `hasElectron()` so it ONLY appears in the legacy Electron build.
/// The Tauri build — the one this banner sends users to — never renders it,
/// even though both runtimes share this exact code. There is no in-place
/// auto-update path from Electron to Tauri (electron-updater installs
/// electron-builder artifacts; Tauri verifies a minisign signature the
/// Electron build doesn't carry), so the migration is a one-time manual
/// re-download. Dismissal persists so we nudge rather than nag.
export const MigrationBanner: React.FC = () => {
  const [dismissed, setDismissed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(DISMISS_KEY) === "1";
    } catch {
      return false;
    }
  });

  if (!hasElectron() || dismissed) {
    return null;
  }

  const dismiss = () => {
    try {
      localStorage.setItem(DISMISS_KEY, "1");
    } catch {
      // localStorage unavailable — dismiss for this session only
    }
    setDismissed(true);
  };

  return (
    <div
      data-testid="migration-banner"
      role="alert"
      className="flex items-center gap-3 px-4 py-2 bg-surface-raised border-b border-line"
    >
      <Download size={16} aria-hidden="true" className="text-accent shrink-0" />
      <div className="flex-1 min-w-0 text-xs font-mono">
        <span className="text-accent font-semibold">
          A new version of Pollis is available.
        </span>
        <span className="text-dim">
          {" "}This build no longer updates automatically — download the latest
          version to keep getting updates.
        </span>
      </div>
      <Button
        size="sm"
        variant="primary"
        onClick={() => {
          void shellOpen(DOWNLOAD_URL);
        }}
      >
        Download
      </Button>
      <button
        type="button"
        onClick={dismiss}
        aria-label="Dismiss update notice"
        className="icon-btn-sm shrink-0 text-dim"
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
};
