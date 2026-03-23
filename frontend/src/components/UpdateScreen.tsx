import React, { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-shell";

interface LatestJson {
  version: string;
  notes?: string;
  macos?: string;
  windows?: string;
  linux?: string;
}

interface Props {
  currentVersion: string;
  latest: LatestJson;
}

function getPlatformUrl(latest: LatestJson): string | null {
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("mac")) {
    return latest.macos ?? null;
  }
  if (ua.includes("win")) {
    return latest.windows ?? null;
  }
  return latest.linux ?? null;
}

function parseNotes(notes: string): string[] {
  return notes
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
}

export const UpdateScreen: React.FC<Props> = ({ currentVersion, latest }) => {
  const latestVersion = latest.version.replace(/^v/, "");
  const notes = latest.notes ? parseNotes(latest.notes) : [];
  const downloadUrl = getPlatformUrl(latest);

  const [visibleNotes, setVisibleNotes] = useState<string[]>([]);
  const [showDownload, setShowDownload] = useState(false);

  // Print notes line by line with a short delay
  useEffect(() => {
    if (notes.length === 0) {
      setShowDownload(true);
      return;
    }
    let i = 0;
    const interval = setInterval(() => {
      setVisibleNotes((prev) => [...prev, notes[i]]);
      i += 1;
      if (i >= notes.length) {
        clearInterval(interval);
        setTimeout(() => setShowDownload(true), 300);
      }
    }, 120);
    return () => clearInterval(interval);
  }, []);

  const handleDownload = async () => {
    if (!downloadUrl) {
      return;
    }
    await open(downloadUrl);
  };

  return (
    <div
      data-testid="update-screen"
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
          maxWidth: 480,
          padding: "2rem",
          display: "flex",
          flexDirection: "column",
          gap: "1.25rem",
        }}
      >
        <span style={{ color: "var(--c-accent)", fontSize: "0.875rem", fontWeight: 700 }}>
          Pollis.
        </span>

        <div style={{ display: "flex", flexDirection: "column", gap: "0.25rem" }}>
          <span style={{ color: "var(--c-text-muted)", fontSize: "0.75rem" }}>
            $ pollis --update
          </span>
          <span style={{ color: "var(--c-text)", fontSize: "0.75rem" }}>
            ==&gt; Updating Pollis {currentVersion} → {latestVersion}
          </span>
        </div>

        {notes.length > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: "0.125rem" }}>
            <span style={{ color: "var(--c-text-muted)", fontSize: "0.7rem", marginBottom: "0.25rem" }}>
              Release notes:
            </span>
            {visibleNotes.map((line, i) => (
              <span key={i} style={{ color: "var(--c-text-dim)", fontSize: "0.7rem" }}>
                &gt; {line.replace(/^[-*]\s*/, "")}
              </span>
            ))}
          </div>
        )}

        {showDownload && downloadUrl && (
          <button
            data-testid="update-download-button"
            onClick={handleDownload}
            style={{
              background: "transparent",
              border: "1px solid var(--c-accent)",
              borderRadius: "4px",
              color: "var(--c-accent)",
              fontFamily: "inherit",
              fontSize: "0.75rem",
              padding: "0.5rem 1rem",
              cursor: "pointer",
              alignSelf: "flex-start",
              transition: "background 0.15s",
            }}
            onMouseEnter={(e) => {
              (e.currentTarget as HTMLElement).style.background = "color-mix(in srgb, var(--c-accent) 12%, transparent)";
            }}
            onMouseLeave={(e) => {
              (e.currentTarget as HTMLElement).style.background = "transparent";
            }}
          >
            Download Pollis {latestVersion}
          </button>
        )}

        <span
          style={{
            color: "var(--c-text-muted)",
            fontSize: "0.65rem",
            opacity: 0.6,
          }}
        >
          This update is required to continue.
        </span>
      </div>
    </div>
  );
};
