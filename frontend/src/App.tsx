import React, {
  useEffect,
  useState,
  useCallback,
  useRef,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "./stores/appStore";
import { LoginScreen } from "./components/Auth/LoginScreen";
import { SaveSecretKeyScreen } from "./components/Auth/SaveSecretKeyScreen";
import { EnrollmentGateScreen } from "./components/Auth/EnrollmentGateScreen";
import { EnrollmentApprovalPrompt } from "./components/Auth/EnrollmentApprovalPrompt";
import { TerminalApp } from "./components/TerminalApp";
import { TitleBar } from "./components/Layout/TitleBar";
import { DotMatrix } from "./components/ui/DotMatrix";
import { Card } from "./components/ui/Card";
import { UpdateScreen } from "./components/UpdateScreen";
import * as api from "./services/api";
import { getPreference, applyPreferences } from "./hooks/queries/usePreferences";
import { restoreWindowState, useWindowState } from "./hooks/useWindowState";
import type { User, AccountInfo } from "./types";
import { LoadingSpinner } from "./components/ui/LoaderSpinner";
import { Button } from "./components/ui/Button";

type AppState =
  | "initializing"
  | "loading"
  | "email-auth"
  | "save-secret-key"
  | "enrollment-required"
  | "logout-confirm"
  | "identity-setup"
  | "update-required"
  | "ready";

// Dev-only: expose device list on window.__POLLIS_DEBUG__ for console inspection.
function setupDebugDevices(userId: string) {
  api.listUserDevices(userId).then((devices) => {
    (window as unknown as Record<string, unknown>).__POLLIS_DEBUG__ = { userId, devices };
    console.table(devices.map((d) => ({
      device_id: d.device_id,
      is_current: d.is_current ? "<<< THIS" : "",
      last_seen: d.last_seen,
    })));
  }).catch((err) => {
    console.warn("[debug] failed to fetch devices:", err);
  });
}

function MainApp() {
  const {
    currentUser,
    setCurrentUser,
    pendingEnrollmentApproval,
    setPendingEnrollmentApproval,
  } = useAppStore();

  const [appState, setAppState] = useState<AppState>("initializing");
  const [knownAccounts, setKnownAccounts] = useState<AccountInfo[]>([]);
  // Pending Secret Key (first-device signup) — held in component state ONLY
  // for the duration of the SaveSecretKeyScreen, never persisted.
  const [pendingSecretKey, setPendingSecretKey] = useState<string | null>(null);
  // The user we're enrolling. Set when an enrollment-required login happens
  // so the EnrollmentGateScreen knows whose account it's joining.
  const [pendingEnrollmentUser, setPendingEnrollmentUser] = useState<User | null>(null);


  /// Final phase of any successful sign-in (resume or fresh OTP). Loads
  /// preferences, debug data, and transitions to "ready". Assumes the
  /// user has account_id_key locally — call only AFTER first-device
  /// signup or device enrollment has completed.
  const completeSignIn = useCallback(async (user: User) => {
    try {
      await api.initializeIdentity(user.id);
    } catch (err) {
      console.error("[App] Failed to initialize identity:", err);
    }
    if (import.meta.env.DEV) {
      setupDebugDevices(user.id);
    }
    try {
      const json = await invoke<string>("get_preferences", { userId: user.id });
      const prefs = {
        accent_color: getPreference<string | undefined>(json, "accent_color", undefined),
        background_color: getPreference<string | undefined>(json, "background_color", undefined),
        font_size: getPreference<string | undefined>(json, "font_size", undefined),
      };
      applyPreferences(prefs);
    } catch {
      // Preferences are optional
    }
    setCurrentUser(user);
    setAppState("ready");
  }, [setCurrentUser]);

  const checkStoredSession = useCallback(async () => {
    try {
      // Check for required update before anything else (skip in dev)
      if (!import.meta.env.DEV) {
        const { check: checkUpdate } = await import("@tauri-apps/plugin-updater");
        const update = await checkUpdate();
        if (update) {
          await invoke("mark_update_required");
          setAppState("update-required");
          return;
        }
      }

      const session = await api.getSession();
      if (session) {
        if (session.enrollmentRequired) {
          // Returning device that has never been enrolled (e.g. user
          // signed in once before the enrollment system shipped, or the
          // OS keystore got wiped). Route to the gate before any other
          // UI renders.
          setPendingEnrollmentUser(session.user);
          setAppState("enrollment-required");
          return;
        }
        await completeSignIn(session.user);
      } else {
        // No active session — load known accounts for the login screen
        try {
          const index = await api.listKnownAccounts();
          setKnownAccounts(index.accounts);
        } catch {
          // Non-critical — fall through to email auth without the list
        }
        setAppState("email-auth");
      }
    } catch (error) {
      console.error("[App] Error checking session:", error);
      setAppState("email-auth");
    }
  }, [completeSignIn]);

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

  // Safety net: if currentUser is cleared (e.g. account deletion) but appState
  // is still "ready", redirect to auth so the user isn't stuck on a blank screen.
  useEffect(() => {
    if (appState === "ready" && !currentUser) {
      setAppState("email-auth");
    }
  }, [appState, currentUser]);

  const handleAuthSuccess = useCallback(async (result: api.AuthResult) => {
    // Branch 1: first-device signup. Show the Secret Key screen and gate
    // navigation until the user types it back to confirm they saved it.
    if (result.newSecretKey) {
      setPendingSecretKey(result.newSecretKey);
      setPendingEnrollmentUser(result.user);
      setAppState("save-secret-key");
      return;
    }

    // Branch 2: returning device that has no local account_id_key.
    // Must run the enrollment gate before the main app.
    if (result.enrollmentRequired) {
      setPendingEnrollmentUser(result.user);
      setAppState("enrollment-required");
      return;
    }

    // Branch 3: normal returning user. Boot straight to identity setup.
    setAppState("identity-setup");
    await completeSignIn(result.user);
  }, [completeSignIn]);

  const handleSecretKeySaved = useCallback(async () => {
    setPendingSecretKey(null);
    if (pendingEnrollmentUser) {
      const user = pendingEnrollmentUser;
      setPendingEnrollmentUser(null);
      setAppState("identity-setup");
      await completeSignIn(user);
    } else {
      setAppState("email-auth");
    }
  }, [pendingEnrollmentUser, completeSignIn]);

  const handleEnrolled = useCallback(async () => {
    if (pendingEnrollmentUser) {
      const user = pendingEnrollmentUser;
      setPendingEnrollmentUser(null);
      setAppState("identity-setup");
      await completeSignIn(user);
    } else {
      setAppState("email-auth");
    }
  }, [pendingEnrollmentUser, completeSignIn]);

  const handleEnrollmentCancelled = useCallback(async () => {
    setPendingEnrollmentUser(null);
    setPendingSecretKey(null);
    try {
      await api.logout(false);
    } catch (err) {
      console.error("[App] logout during enrollment cancel failed:", err);
    }
    useAppStore.getState().logout();
    try {
      const index = await api.listKnownAccounts();
      setKnownAccounts(index.accounts);
    } catch {
      // Non-critical
    }
    setAppState("email-auth");
  }, []);

  // Navigate to the confirmation view instead of using window.confirm
  const handleLogout = useCallback(() => {
    setAppState("logout-confirm");
  }, []);

  // After delete_account succeeds in Settings, transition to auth screen.
  // Zustand logout() is called in Settings.tsx before this fires.
  const handleDeleteAccount = useCallback(() => {
    setAppState("email-auth");
  }, []);

  const handleLogoutConfirm = useCallback(async (deleteData: boolean) => {
    try {
      await api.logout(deleteData);
    } catch (error) {
      console.error("Failed to logout:", error);
    }
    useAppStore.getState().logout();
    // Re-fetch known accounts so the login screen shows the switcher
    try {
      const index = await api.listKnownAccounts();
      setKnownAccounts(index.accounts);
    } catch {
      // Non-critical
    }
    setAppState("email-auth");
  }, []);

  if (appState === "update-required") {
    return (
      <div style={{ height: "100%", width: "100%", display: "flex", flexDirection: "column" }}>
        <TitleBar />
        <UpdateScreen />
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
      <LoginScreen
        knownAccounts={knownAccounts}
        onAuthSuccess={handleAuthSuccess}
        onWipeComplete={() => setKnownAccounts([])}
      />
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

              <div className="flex flex-col gap-3">
                <Button
                  data-testid="logout-delete-data-button"
                  onClick={() => handleLogoutConfirm(true)}
                  variant="danger"
                  className="w-full"
                >
                  Delete data and sign out
                </Button>
                <Button
                  data-testid="logout-keep-data-button"
                  onClick={() => handleLogoutConfirm(false)}
                  className="w-full"
                >
                  Keep data and sign out
                </Button>
                <Button
                  data-testid="logout-cancel-button"
                  onClick={() => setAppState("ready")}
                  variant="ghost"
                  className="w-full"
                >
                  Cancel
                </Button>
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
          <Card padding="lg" style={{ width: "clamp(280px, 100%, 400px)" }}>
            <div className="flex flex-col gap-3">
              <span
                data-testid="identity-setup-message"
                className="font-mono font-semibold"
                style={{ color: "var(--c-accent)" }}
              >
                Welcome to Pollis
              </span>
              <p className="text-xs font-mono flex items-center gap-2" style={{ color: "var(--c-text)" }}>
                <span>
                  Setting up your encrypted identity
                </span>
                <LoadingSpinner size="sm" />
              </p>
            </div>
          </Card>
        </div>
      </div>
    );
  }

  if (appState === "save-secret-key" && pendingSecretKey) {
    return (
      <SaveSecretKeyScreen
        secretKey={pendingSecretKey}
        username={pendingEnrollmentUser?.username ?? currentUser?.username ?? null}
        onConfirmed={handleSecretKeySaved}
      />
    );
  }

  if (appState === "enrollment-required" && pendingEnrollmentUser) {
    return (
      <EnrollmentGateScreen
        userId={pendingEnrollmentUser.id}
        userEmail={pendingEnrollmentUser.email ?? ""}
        onEnrolled={handleEnrolled}
        onCancel={handleEnrollmentCancelled}
        onResetComplete={(newKey) => {
          // Soft recovery succeeded. The new Secret Key must be shown
          // once before the user reaches the main app — reuse the
          // first-device SaveSecretKeyScreen.
          setPendingSecretKey(newKey);
          setAppState("save-secret-key");
        }}
      />
    );
  }

  if (appState === "ready" && currentUser) {
    return (
      <div
        data-testid="app-ready"
        style={{ height: "100%", width: "100%", display: "flex", flexDirection: "column", overflow: "hidden", position: "relative" }}
      >
        <div style={{ flex: 1, overflow: "hidden" }}>
          <TerminalApp onLogout={handleLogout} onDeleteAccount={handleDeleteAccount} />
        </div>
        {/* Global enrollment-approval takeover. Layered above the main app
            UI so the user MUST act on it before continuing. The overlay
            element itself is fixed-position. */}
        {pendingEnrollmentApproval && (
          <EnrollmentApprovalPrompt
            requestId={pendingEnrollmentApproval.requestId}
            newDeviceId={pendingEnrollmentApproval.newDeviceId}
            verificationCode={pendingEnrollmentApproval.verificationCode}
            onResolved={() => setPendingEnrollmentApproval(null)}
          />
        )}
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

export default MainApp;
