import React, {
  useEffect,
  useState,
  useCallback,
  useRef,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { useAppStore } from "./stores/appStore";
import { EmailOTPAuth } from "./components/Auth/EmailOTPAuth";
import { TerminalApp } from "./components/TerminalApp";
import { TitleBar } from "./components/Layout/TitleBar";
import { DotMatrix } from "./components/ui/DotMatrix";
import { Card } from "./components/ui/Card";
import { UpdateScreen } from "./components/UpdateScreen";
import * as api from "./services/api";
import { getPreference, applyPreferences } from "./hooks/queries/usePreferences";
import { restoreWindowState, useWindowState } from "./hooks/useWindowState";
import type { User } from "./types";

interface LatestJson {
  version: string;
  notes?: string;
  macos?: string;
  windows?: string;
  linux?: string;
}

type AppState = "initializing" | "loading" | "email-auth" | "logout-confirm" | "identity-setup" | "update-required" | "ready";

function semverIsOutdated(current: string, latest: string): boolean {
  const parse = (v: string) => v.replace(/^v/, "").split(".").map(Number);
  const [cMaj, cMin, cPatch] = parse(current);
  const [lMaj, lMin, lPatch] = parse(latest);
  if (lMaj !== cMaj) {
    return lMaj > cMaj;
  }
  if (lMin !== cMin) {
    return lMin > cMin;
  }
  return lPatch > cPatch;
}

async function fetchLatest(): Promise<LatestJson | null> {
  try {
    const res = await fetch("https://cdn.pollis.com/releases/latest.json");
    return await res.json() as LatestJson;
  } catch {
    return null;
  }
}

function MainApp() {
  const {
    currentUser,
    setCurrentUser,
  } = useAppStore();

  const [appState, setAppState] = useState<AppState>("initializing");
  const [latestJson, setLatestJson] = useState<LatestJson | null>(null);
  const [currentVersion, setCurrentVersion] = useState<string>("");

  const checkStoredSession = useCallback(async () => {
    try {
      // Check for required update before anything else
      const [latest, version] = await Promise.all([fetchLatest(), getVersion()]);
      setCurrentVersion(version);
      if (latest && semverIsOutdated(version, latest.version)) {
        setLatestJson(latest);
        await invoke("mark_update_required");
        setAppState("update-required");
        return;
      }

      const user = await api.getSession();
      if (user) {
        try {
          await api.initializeIdentity(user.id);
        } catch (err) {
          console.error("[App] Failed to initialize identity:", err);
        }
        // Load and apply saved preferences before showing the UI
        try {
          const json = await invoke<string>("get_preferences", { userId: user.id });
          const prefs = {
            accent_color: getPreference<string | undefined>(json, "accent_color", undefined),
            font_size: getPreference<string | undefined>(json, "font_size", undefined),
          };
          applyPreferences(prefs);
        } catch {
          // Preferences are optional — ignore failures
        }
        setCurrentUser(user);
        setAppState("ready");
      } else {
        setAppState("email-auth");
      }
    } catch (error) {
      console.error("[App] Error checking session:", error);
      setAppState("email-auth");
    }
  }, [setCurrentUser]);

  const hasInitializedRef = useRef(false);

  useEffect(() => {
    if (hasInitializedRef.current) {
      return;
    }
    hasInitializedRef.current = true;
    restoreWindowState();
    checkStoredSession();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Escape on logout-confirm goes back to ready
  useEffect(() => {
    if (appState !== "logout-confirm") {
      return;
    }
    const handle = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setAppState("ready");
      }
    };
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  }, [appState]);

  useWindowState();

  const handleAuthSuccess = useCallback(async (user: User) => {
    // Show the identity setup loading screen while keys are generated/uploaded
    setAppState("identity-setup");
    try {
      await api.initializeIdentity(user.id);
    } catch (err) {
      console.error("[App] Failed to initialize identity:", err);
    }
    // Apply saved preferences for newly logged-in user
    try {
      const json = await invoke<string>("get_preferences", { userId: user.id });
      const prefs = {
        accent_color: getPreference<string | undefined>(json, "accent_color", undefined),
        font_size: getPreference<string | undefined>(json, "font_size", undefined),
      };
      applyPreferences(prefs);
    } catch {
      // Preferences are optional
    }
    setCurrentUser(user);
    setAppState("ready");
  }, [setCurrentUser]);

  // Navigate to the confirmation view instead of using window.confirm
  const handleLogout = useCallback(() => {
    setAppState("logout-confirm");
  }, []);

  // After delete_account succeeds in Settings, clear local state and go to auth
  const handleDeleteAccount = useCallback(() => {
    setAppState("email-auth");
    useAppStore.getState().logout();
  }, []);

  const handleLogoutConfirm = useCallback(async (deleteData: boolean) => {
    try {
      await api.logout(deleteData);
    } catch (error) {
      console.error("Failed to logout:", error);
    }
    setAppState("email-auth");
    useAppStore.getState().logout();
  }, []);

  if (appState === "update-required" && latestJson) {
    return (
      <div style={{ height: "100%", width: "100%", display: "flex", flexDirection: "column" }}>
        <TitleBar />
        <UpdateScreen currentVersion={currentVersion} latest={latestJson} />
      </div>
    );
  }

  if (appState === "initializing") {
    return (
      <div
        data-testid="loading-screen"
        className="flex items-center justify-center h-full w-full"
        style={{ background: "var(--c-bg)" }}
      >
        <span
          data-testid="loading-spinner"
          className="text-xs font-mono"
          style={{ color: "var(--c-text-muted)" }}
        >
          initializing…
        </span>
      </div>
    );
  }

  if (appState === "email-auth") {
    return (
      <div
        data-testid="auth-screen"
        className="flex flex-col h-full w-full"
        style={{ background: "var(--c-bg)", position: "relative" }}
      >
        {/* DotMatrix background — low opacity, random algorithm each load */}
        <div style={{ position: "absolute", inset: 0, opacity: 0.35, pointerEvents: "none" }}>
          <DotMatrix />
        </div>

        {/* Title bar with proper window controls */}
        <TitleBar />

        {/* Centered auth card */}
        <div className="flex-1 flex items-center justify-center" style={{ position: "relative", zIndex: 1 }}>
          <Card padding="lg" style={{ width: "100%", maxWidth: 360 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h1 className="text-base font-mono font-bold" style={{ color: "var(--c-accent)" }}>
                  Pollis.
                </h1>
                <p className="text-xs mt-1 font-mono" style={{ color: "var(--c-text-muted)" }}>
                  Enter your email to continue
                </p>
              </div>
              <EmailOTPAuth onSuccess={handleAuthSuccess} />
            </div>
          </Card>
        </div>
      </div>
    );
  }

  if (appState === "logout-confirm") {
    return (
      <div
        data-testid="logout-confirm-screen"
        className="flex flex-col h-full w-full"
        style={{ background: "var(--c-bg)", position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
          <DotMatrix speed={0.2} />
        </div>

        <TitleBar />

        <div className="flex-1 flex items-center justify-center" style={{ position: "relative", zIndex: 1 }}>
          <Card padding="lg" style={{ width: "100%", maxWidth: 360 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h2 className="text-sm font-mono font-semibold" style={{ color: "var(--c-text)" }}>
                  Sign out
                </h2>
                <p className="text-xs mt-1 font-mono" style={{ color: "var(--c-text-muted)" }}>
                  Do you want to delete your locally stored messages and keys?
                </p>
              </div>

              <div className="flex flex-col gap-2">
                <button
                  data-testid="logout-delete-data-button"
                  onClick={() => handleLogoutConfirm(true)}
                  className="w-full py-2 px-4 font-mono text-xs transition-colors"
                  style={{
                    background: "transparent",
                    border: "1px solid hsl(0 70% 50% / 40%)",
                    borderRadius: "4px",
                    color: "hsl(0 70% 65%)",
                  }}
                  onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = "hsl(0 70% 50% / 10%)"; }}
                  onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "transparent"; }}
                >
                  Delete data and sign out
                </button>
                <button
                  data-testid="logout-keep-data-button"
                  onClick={() => handleLogoutConfirm(false)}
                  className="w-full py-2 px-4 font-mono text-xs transition-colors"
                  style={{
                    background: "transparent",
                    border: "1px solid var(--c-border)",
                    borderRadius: "4px",
                    color: "var(--c-text-dim)",
                  }}
                  onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = "var(--c-hover)"; }}
                  onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "transparent"; }}
                >
                  Keep data and sign out
                </button>
                <button
                  data-testid="logout-cancel-button"
                  onClick={() => setAppState("ready")}
                  className="w-full py-1 font-mono text-xs"
                  style={{ color: "var(--c-text-muted)" }}
                >
                  Cancel
                </button>
              </div>
            </div>
          </Card>
        </div>
      </div>
    );
  }

  if (appState === "identity-setup") {
    return (
      <div
        data-testid="identity-setup-screen"
        className="flex flex-col h-full w-full"
        style={{ background: "var(--c-bg)", position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.35, pointerEvents: "none" }}>
          <DotMatrix />
        </div>
        <TitleBar />
        <div className="flex-1 flex items-center justify-center" style={{ position: "relative", zIndex: 1 }}>
          <Card padding="lg" style={{ width: "100%", maxWidth: 360 }}>
            <div className="flex flex-col gap-3">
              <span
                data-testid="identity-setup-message"
                className="text-sm font-mono font-semibold"
                style={{ color: "var(--c-accent)" }}
              >
                Pollis.
              </span>
              <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
                Setting up your encrypted identity
                <IdentitySetupDots />
              </p>
            </div>
          </Card>
        </div>
      </div>
    );
  }

  if (appState === "ready" && currentUser) {
    return (
      <div
        data-testid="app-ready"
        style={{ height: "100%", width: "100%", display: "flex", flexDirection: "column", overflow: "hidden", position: "relative" }}
      >
        <div style={{ flex: 1, overflow: "hidden" }}>
          <TerminalApp onLogout={handleLogout} />
        </div>
      </div>
    );
  }

  // Fallback loading
  return (
    <div
      data-testid="loading-screen"
      className="flex items-center justify-center h-full w-full"
      style={{ background: "var(--c-bg)" }}
    >
      <span
        data-testid="loading-spinner"
        className="text-xs font-mono"
        style={{ color: "var(--c-text-muted)" }}
      >
        loading…
      </span>
    </div>
  );
}

// ── Animated dots for the identity setup loading screen ──────────────────────

const IdentitySetupDots: React.FC = () => {
  const [count, setCount] = useState(0);

  useEffect(() => {
    const interval = setInterval(() => {
      setCount((prev) => (prev + 1) % 4);
    }, 400);
    return () => clearInterval(interval);
  }, []);

  return <span aria-hidden="true">{".".repeat(count)}</span>;
};

export default MainApp;
