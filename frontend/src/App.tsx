import React, {
  useEffect,
  useState,
  useCallback,
  useRef,
} from "react";
import { useTranslation } from "react-i18next";
import { invoke, listen, onDeviceRevoked, deviceWasRevoked } from "./bridge";
import { observer } from "mobx-react-lite";
import { appStore } from "./stores/appStore";
import { LoginScreen } from "./components/Auth/LoginScreen";
import { SaveSecretKeyScreen } from "./components/Auth/SaveSecretKeyScreen";
import { PinCreateScreen } from "./components/Auth/PinCreateScreen";
import { PinEntryScreen } from "./components/Auth/PinEntryScreen";
import { EnrollmentGateScreen } from "./components/Auth/EnrollmentGateScreen";
import { EnrollmentApprovalPrompt } from "./components/Auth/EnrollmentApprovalPrompt";
import { TerminalApp } from "./components/TerminalApp";
import { TitleBar } from "./components/Layout/TitleBar";
import { DotMatrix } from "./components/ui/DotMatrix";
import { Card } from "./components/ui/Card";
import { UpdateScreen } from "./components/UpdateScreen";
import type { ManagedInstallInfo } from "./types";
import * as api from "./services/api";
import { getPreference, applyPreferences, applyDeviceFontSize, useApplyPreferences, usePreferences } from "./hooks/queries/usePreferences";
import { restoreWindowState, useWindowState } from "./hooks/useWindowState";
import type { User, AccountInfo } from "./types";
import { LoadingSpinner } from "./components/ui/LoaderSpinner";
import { Button } from "./components/ui/Button";
import { useQueryClient } from "@tanstack/react-query";
import { installTrayVoiceBridge, installVoiceBridge } from "./voice";
import { clearAllDrafts } from "./utils/drafts";
import { AUTO_LOCK_EVENT } from "./utils/autoLock";
import { clearsQueryCache } from "./utils/queryCacheLifecycle";
import { adoptUserLanguage } from "./i18n";

type AppState =
  | "initializing"
  | "loading"
  | "email-auth"
  | "save-secret-key"
  | "pin-create"
  | "pin-entry"
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
  const { t } = useTranslation("auth");
  const {
    currentUser,
    setCurrentUser,
    pendingEnrollmentApproval,
    setPendingEnrollmentApproval,
  } = appStore;
  const updateRequired = appStore.updateRequired;

  const [appState, setAppState] = useState<AppState>("initializing");
  const [knownAccounts, setKnownAccounts] = useState<AccountInfo[]>([]);
  // First-device Secret Key: component state only, never persisted.
  const [pendingSecretKey, setPendingSecretKey] = useState<string | null>(null);
  const [pendingEnrollmentUser, setPendingEnrollmentUser] = useState<User | null>(null);
  // QR device link (#1207): the gate auto-starts with no code; approval happens
  // on the device that showed the QR.
  const [pendingEnrollmentLinked, setPendingEnrollmentLinked] = useState(false);
  // Queued for PIN setup (`pin_set` false) or PIN entry (set, not entered this boot).
  const [pendingPinUser, setPendingPinUser] = useState<User | null>(null);
  // Why the last session ended when the user did not end it (#1338).
  const [sessionNotice, setSessionNotice] = useState<string | null>(null);


  // Final phase of any successful sign-in. Call only AFTER first-device signup
  // or device enrollment, once account_id_key is local.
  const completeSignIn = useCallback(async (user: User) => {
    // Adopt this user's device-local language before the shell renders, so a
    // shared OS account doesn't show the previous user's language.
    await adoptUserLanguage(user.id);
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
        shortcut_overrides: getPreference<{ [commandId: string]: string } | undefined>(
          json,
          "shortcut_overrides",
          undefined,
        ),
      };
      applyPreferences(prefs);
      // Font size is device-local; the legacy remote field (if any) seeds it once.
      applyDeviceFontSize(user.id, prefs);
    } catch {
      // Preferences are optional
    }
    setCurrentUser(user);
    setAppState("ready");
  }, [setCurrentUser]);

  const checkStoredSession = useCallback(async () => {
    try {
      // A package manager (AUR, …) owns some installs, so the in-app updater
      // can't replace the binary: skip the gate; the poller still badges
      // updates. Dev builds skip it all.
      if (!import.meta.env.DEV) {
        const managed = await invoke<ManagedInstallInfo | null>(
          "detect_managed_install",
        ).catch(() => null);
        if (!managed) {
          const { check: checkUpdate } = await import("./bridge");
          const update = await checkUpdate();
          if (update) {
            await invoke("mark_update_required");
            setAppState("update-required");
            return;
          }
        }
      }

      const session = await api.getSession();
      if (session) {
        // A device revoked while offline never got the "device_revoked" nudge.
        // Errors (offline) fall through so a blip never signs anyone out.
        const stillRegistered = await api
          .isCurrentDeviceRegistered(session.user.id)
          .catch(() => true);
        if (!stillRegistered) {
          deviceWasRevoked();
          return;
        }

        if (session.enrollmentRequired) {
          // Never enrolled (pre-enrollment sign-in, or wiped keystore).
          setPendingEnrollmentUser(session.user);
          setAppState("enrollment-required");
          return;
        }

        // `is_unlocked` is memory-only, so it resets every boot. Pre-PIN
        // upgrades land with `pin_set=false` and go to the create screen.
        const unlockState = await api.getUnlockState();
        if (unlockState.pin_set && !unlockState.is_unlocked) {
          setPendingPinUser(session.user);
          setAppState("pin-entry");
          return;
        }
        if (!unlockState.pin_set) {
          setPendingPinUser(session.user);
          setAppState("pin-create");
          return;
        }

        await completeSignIn(session.user);
      } else {
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

  // Re-apply visual preferences whenever the prefs query resolves, including
  // reopen with a stored session where completeSignIn's fetch could race.
  useApplyPreferences();

  const queryClient = useQueryClient();
  const prefsQuery = usePreferences().query;
  const prefsRef = useRef(prefsQuery.data);
  prefsRef.current = prefsQuery.data;
  useEffect(() => {
    const handle = installVoiceBridge({
      queryClient,
      preferencesProvider: () => prefsRef.current,
    });
    const trayHandle = installTrayVoiceBridge();
    return () => {
      handle.dispose();
      trayHandle.dispose();
    };
  }, [queryClient]);

  // If currentUser is cleared (e.g. account deletion) while "ready", go to auth.
  useEffect(() => {
    if (appState === "ready" && !currentUser) {
      setAppState("email-auth");
    }
  }, [appState, currentUser]);

  // Drop every in-memory draft on any change of active user id. Drafts are never
  // persisted, so a second user on the same machine sees an empty composer.
  useEffect(() => {
    clearAllDrafts();
  }, [currentUser?.id]);

  // Locking must empty the React Query cache: decrypted message bodies would
  // otherwise outlive `pin::lock` in the module-level cache. Keyed on entering
  // "pin-entry" (Cmd/Ctrl+L, idle auto-lock #851, boot gate) and "email-auth"
  // (every session end, #1256), after AppShell has unmounted.
  useEffect(() => {
    if (clearsQueryCache(appState)) {
      queryClient.clear();
    }
  }, [appState, queryClient]);

  const handleAuthSuccess = useCallback(async (result: api.AuthResult) => {
    // First-device signup: gate navigation until the Secret Key is typed back.
    if (result.newSecretKey) {
      setPendingSecretKey(result.newSecretKey);
      setPendingEnrollmentUser(result.user);
      setAppState("save-secret-key");
      return;
    }

    // Returning device with no local account_id_key: enrollment gate first.
    if (result.enrollmentRequired) {
      setPendingEnrollmentUser(result.user);
      setPendingEnrollmentLinked(result.linked ?? false);
      setAppState("enrollment-required");
      return;
    }

    try {
      const unlockState = await api.getUnlockState();
      setPendingPinUser(result.user);
      setAppState(unlockState.pin_set ? "pin-entry" : "pin-create");
    } catch (err) {
      console.error("[App] getUnlockState failed, falling back to PIN create:", err);
      setPendingPinUser(result.user);
      setAppState("pin-create");
    }
  }, []);

  const handleSecretKeySaved = useCallback(async () => {
    setPendingSecretKey(null);
    if (pendingEnrollmentUser) {
      // `initialize_identity` has run, so `set_pin` can wrap account_id_key.
      const user = pendingEnrollmentUser;
      setPendingEnrollmentUser(null);
      setPendingPinUser(user);
      setAppState("pin-create");
    } else {
      setAppState("email-auth");
    }
  }, [pendingEnrollmentUser]);

  const handleEnrolled = useCallback(async () => {
    if (pendingEnrollmentUser) {
      const user = pendingEnrollmentUser;
      setPendingEnrollmentUser(null);
      setPendingPinUser(user);
      setAppState("pin-create");
    } else {
      setAppState("email-auth");
    }
  }, [pendingEnrollmentUser]);

  const handlePinCreated = useCallback(async () => {
    if (pendingPinUser) {
      const user = pendingPinUser;
      setPendingPinUser(null);
      setAppState("identity-setup");
      // After set_pin, so the local DB is open. Idempotent for fresh signup;
      // required for enrollment / recovery / identity reset so the device cert
      // is published and the device external-joins existing groups + DMs.
      try {
        await api.finalizeDeviceEnrollment(user.id);
      } catch (err) {
        console.error("[App] finalizeDeviceEnrollment failed:", err);
      }
      await completeSignIn(user);
    } else {
      setAppState("email-auth");
    }
  }, [pendingPinUser, completeSignIn]);

  const handlePinUnlocked = useCallback(async () => {
    if (pendingPinUser) {
      const user = pendingPinUser;
      setPendingPinUser(null);
      setAppState("identity-setup");
      await completeSignIn(user);
    } else {
      setAppState("email-auth");
    }
  }, [pendingPinUser, completeSignIn]);

  const handlePinForgot = useCallback(async () => {
    // "Forgot PIN" runs Secret-Key recovery as an enrollment flow; the backend
    // wipes the wrapped blobs on success and a new PIN is set at the end.
    if (pendingPinUser) {
      setPendingEnrollmentUser(pendingPinUser);
      setPendingPinUser(null);
      setAppState("enrollment-required");
    } else {
      setAppState("email-auth");
    }
  }, [pendingPinUser]);

  const handlePinSwitchAccount = useCallback(async () => {
    setPendingPinUser(null);
    try {
      await api.logout(false);
    } catch (err) {
      console.error("[App] logout during pin switch-account failed:", err);
    }
    appStore.logout();
    try {
      const index = await api.listKnownAccounts();
      setKnownAccounts(index.accounts);
    } catch {
      // Non-critical
    }
    setAppState("email-auth");
  }, []);

  const handleEnrollmentCancelled = useCallback(async () => {
    setPendingEnrollmentUser(null);
    setPendingSecretKey(null);
    try {
      await api.logout(false);
    } catch (err) {
      console.error("[App] logout during enrollment cancel failed:", err);
    }
    appStore.logout();
    try {
      const index = await api.listKnownAccounts();
      setKnownAccounts(index.accounts);
    } catch {
      // Non-critical
    }
    setAppState("email-auth");
  }, []);

  const handleLogout = useCallback(() => {
    setAppState("logout-confirm");
  }, []);

  // Cmd/Ctrl+L: clear unlock state and return to PIN entry, keeping the
  // session. AppShell unmounting closes LiveKit rooms.
  const handleLock = useCallback(async () => {
    if (!currentUser) {
      return;
    }
    try {
      await api.lockUnlock();
    } catch (err) {
      console.error("[App] lock failed:", err);
    }
    setPendingPinUser(currentUser);
    setAppState("pin-entry");
  }, [currentUser]);

  // Idle auto-lock (#851): Rust has already locked; reuse `handleLock` so there
  // is one lock path.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void listen(AUTO_LOCK_EVENT, () => {
      void handleLock();
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [handleLock]);

  const handleDeleteAccount = useCallback(() => {
    setAppState("email-auth");
  }, []);

  const handleLogoutConfirm = useCallback(async (deleteData: boolean) => {
    try {
      await api.logout(deleteData);
    } catch (error) {
      console.error("Failed to logout:", error);
    }
    appStore.logout();
    try {
      const index = await api.listKnownAccounts();
      setKnownAccounts(index.accounts);
    } catch {
      // Non-critical
    }
    setAppState("email-auth");
  }, []);

  // #1338: this device was revoked from another device. End the session and
  // say why on the sign-in screen.
  useEffect(
    () =>
      onDeviceRevoked(async () => {
        // A late rejection may follow a sign-out or another account's sign-in:
        // act only if THIS device is the revoked one.
        const revoked = await invoke<boolean>("is_current_device_revoked").catch(() => false);
        if (!revoked) {
          return;
        }
        console.warn("[App] this device was removed from the account — signing out");
        try {
          await api.logout(false);
        } catch (error) {
          console.error("Failed to logout a revoked device:", error);
        }
        appStore.logout();
        setPendingPinUser(null);
        setPendingEnrollmentUser(null);
        setPendingSecretKey(null);
        try {
          const index = await api.listKnownAccounts();
          setKnownAccounts(index.accounts);
        } catch {
          // Non-critical
        }
        setSessionNotice(t("login.deviceRemoved"));
        setAppState("email-auth");
      }),
    [t],
  );

  if (appState === "update-required" || updateRequired) {
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
        className="flex items-center justify-center h-full w-full bg-bg"
      >
        <span
          data-testid="loading-spinner"
          className="text-xs font-mono text-muted"
        >
          {t("shell.initializing")}
        </span>
      </div>
    );
  }

  if (appState === "email-auth") {
    return (
      <LoginScreen
        knownAccounts={knownAccounts}
        onAuthSuccess={(result) => {
          setSessionNotice(null);
          return handleAuthSuccess(result);
        }}
        onWipeComplete={() => setKnownAccounts([])}
        notice={sessionNotice}
      />
    );
  }

  if (appState === "logout-confirm") {
    return (
      <div
        data-testid="logout-confirm-screen"
        className="flex flex-col h-full w-full bg-bg"
        style={{ position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
          <DotMatrix speed={0.2} />
        </div>

        <TitleBar />

        <div className="flex-1 flex items-center justify-center" style={{ position: "relative", zIndex: 1 }}>
          <Card padding="lg" style={{ width: "100%", maxWidth: 360 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h2 className="text-sm font-mono font-semibold text-fg">
                  {t("shell.signOutTitle")}
                </h2>
                <p className="text-xs mt-1 font-mono text-muted">
                  {t("shell.signOutQuestion")}
                </p>
              </div>

              <div className="flex flex-col gap-3">
                <Button
                  data-testid="logout-delete-data-button"
                  onClick={() => handleLogoutConfirm(true)}
                  variant="danger"
                  className="w-full"
                >
                  {t("shell.signOutDelete")}
                </Button>
                <Button
                  data-testid="logout-keep-data-button"
                  onClick={() => handleLogoutConfirm(false)}
                  className="w-full"
                >
                  {t("shell.signOutKeep")}
                </Button>
                <Button
                  data-testid="logout-cancel-button"
                  onClick={() => setAppState("ready")}
                  variant="ghost"
                  className="w-full"
                >
                  {t("common:actions.cancel")}
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
        className="flex flex-col h-full w-full bg-bg"
        style={{ position: "relative" }}
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
                className="font-mono font-semibold text-accent"
              >
                {t("shell.welcome")}
              </span>
              <p className="text-xs font-mono flex items-center gap-2 text-fg">
                <span>
                  {t("shell.gettingReady")}
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
        email={pendingEnrollmentUser?.email ?? currentUser?.email ?? null}
        onConfirmed={handleSecretKeySaved}
      />
    );
  }

  if (appState === "pin-create" && pendingPinUser) {
    return (
      <PinCreateScreen
        onCreated={handlePinCreated}
        headline={t("pinCreate.headline")}
        subline={t("pinCreate.subline")}
      />
    );
  }

  if (appState === "pin-entry" && pendingPinUser) {
    return (
      <PinEntryScreen
        userId={pendingPinUser.id}
        username={pendingPinUser.username}
        onUnlocked={handlePinUnlocked}
        onForgotPin={handlePinForgot}
        onSwitchAccount={handlePinSwitchAccount}
      />
    );
  }

  if (appState === "enrollment-required" && pendingEnrollmentUser) {
    return (
      <EnrollmentGateScreen
        linked={pendingEnrollmentLinked}
        userId={pendingEnrollmentUser.id}
        userEmail={pendingEnrollmentUser.email ?? ""}
        onEnrolled={handleEnrolled}
        onCancel={handleEnrollmentCancelled}
        onResetComplete={(newKey) => {
          // Show the new Secret Key once before the main app.
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
          <TerminalApp onLogout={handleLogout} onLock={handleLock} onDeleteAccount={handleDeleteAccount} />
        </div>
        {pendingEnrollmentApproval && (
          <EnrollmentApprovalPrompt
            requestId={pendingEnrollmentApproval.requestId}
            newDeviceId={pendingEnrollmentApproval.newDeviceId}
            onResolved={() => setPendingEnrollmentApproval(null)}
          />
        )}
      </div>
    );
  }

  return (
    <div
      data-testid="loading-screen"
      className="flex items-center justify-center h-full w-full bg-bg"
    >
      <span
        data-testid="loading-spinner"
        className="text-xs font-mono text-muted"
      >
        {t("shell.loading")}
      </span>
    </div>
  );
}

export default observer(MainApp);
