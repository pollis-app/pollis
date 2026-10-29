import React, { useEffect, useState, useMemo, useCallback, lazy, Suspense } from "react";
import { useTranslation } from "react-i18next";
import { Outlet, useRouter, useRouterState } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { invoke, getCurrentWindow, hideWindow } from "../../bridge";
import { TitleBar } from "./TitleBar";
import { WindowResizeEdges } from "./WindowResizeEdges";
import { BreadcrumbNav } from "./BreadcrumbNav";
import { Sidebar } from "./Sidebar";
import { RightPanel } from "./RightPanel/RightPanel";
import { useRightPanel } from "./RightPanel/useRightPanel";
import { StatusBarSummary } from "./StatusBarSummary";
import { VoiceBar } from "../Voice/VoiceBar";
import { useSkin } from "../../hooks/queries/usePreferences";
import { useHydrateUnread, useSyncReadCursors } from "../../hooks/queries/useUnread";
import { ScreenShareViewer } from "../Voice/ScreenShareViewer";
import { screenShareSession } from "../../screenshare/screenShareSession";
import { cameraSession } from "../../camera/cameraSession";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { SearchPanel } from "../SearchPanel";
// Lazy: keeps xterm.js + WebGL out of cold launch until first opened (#431).
const TerminalView = lazy(() => import("../TerminalView"));
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { isDropTargetActive, shouldShowDropOverlay } from "../../stores/dropTargetStore";
import { panelWidthStore } from "../../stores/panelWidthStore";
import { historyNavStore } from "../../stores/historyNavStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useLiveKitRealtime } from "../../hooks/useLiveKitRealtime";
import { flushOutbox } from "../../hooks/queries/useMessages";
import { useBadge } from "../../hooks/useBadge";
import { useAutoLock } from "../../hooks/useAutoLock";
import { AlertTriangle, Download, Mail, Phone, X } from "lucide-react";
import { startUpdatePolling, stopUpdatePolling } from "../../services/updatePoller";
import { loadDeviceCallRingtone } from "../../utils/notify";
import { logIgnored } from "../../utils/log";
import { probeRender } from "../../utils/renderProbe";
import { usePreferences } from "../../hooks/queries/usePreferences";
import { voiceSession } from "../../voice";
import { userIdFromVoiceIdentity } from "../../voice/identity";
import { useGlobalShortcut } from "../../keyboard";
import type { RouterContext } from "../../types/router";
import { PressButton } from "../ui/PressButton";

const SIDEBAR_DEFAULT_LS_KEY = "pollis.sidebar_open_by_default";

export const AppShell: React.FC = observer(() => {
  useHydrateUnread(appStore.currentUser?.id ?? null);
  useSyncReadCursors(appStore.currentUser?.id ?? null);
  const { t } = useTranslation("nav");
  probeRender("AppShell");
  const [isSyncing, setIsSyncing] = useState(false);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const [isDragOver, setIsDragOver] = useState(false);
  // Read synchronously from localStorage so there is no flash at mount; the
  // preference query re-mirrors it. Cmd/Ctrl+B and the collapse handle change
  // only this session's state, never the default.
  const [isSidebarOpen, setIsSidebarOpen] = useState<boolean>(() => {
    if (typeof window === "undefined") {
      return true;
    }
    const stored = window.localStorage.getItem(SIDEBAR_DEFAULT_LS_KEY);
    if (stored === "true") {
      return true;
    }
    if (stored === "false") {
      return false;
    }
    return true;
  });
  const queryClient = useQueryClient();
  const router = useRouter();

  const {
    setGroups,
    setChannels,
    voiceState,
    voiceParticipants,
    statusBarAlert,
    setStatusBarAlert,
    voiceError,
    setVoiceError,
    // Do not destructure `isLocalSpeaking`: in this observer the read alone
    // re-renders the whole shell on every utterance (#874).
    incomingCall,
    setIncomingCall,
    viewingScreenShareTrackKey,
    setViewingScreenShareTrackKey,
    shareStopped,
    availableUpdateVersion,
  } = appStore;
  const activeVoiceChannelId =
    voiceState.kind === 'idle' ? null : voiceState.channelId;
  const screenShareError =
    voiceState.kind === 'joined' && voiceState.share.kind === 'failed'
      ? voiceState.share.error
      : null;

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { query: prefsQuery } = usePreferences();

  // Mirror the preference into localStorage for the next open's init.
  const sidebarDefault = prefsQuery.data?.sidebar_open_by_default;
  useEffect(() => {
    if (sidebarDefault !== undefined && typeof window !== "undefined") {
      window.localStorage.setItem(
        SIDEBAR_DEFAULT_LS_KEY,
        sidebarDefault ? "true" : "false",
      );
    }
  }, [sidebarDefault]);

  const currentUser = appStore.currentUser;
  const skin = useSkin();
  // Panel open state lives in `useRightPanel`; AppShell only needs the toggle.
  const { toggle: toggleRightPanel } = useRightPanel();

  // Ringtone loop off the incomingCall slot. Rust owns playback (`start_ring` /
  // `stop_ring`) so re-render churn can't break it; needs both the device-local
  // ringtone toggle and allow_sound_effects.
  useEffect(() => {
    if (!incomingCall) {
      invoke("stop_ring").catch(logIgnored);
      return;
    }
    const allowGlobal = prefsQuery.data?.allow_sound_effects ?? true;
    const allowDevice = loadDeviceCallRingtone(currentUser?.id ?? null);
    if (allowGlobal && allowDevice) {
      invoke("start_ring").catch(logIgnored);
    }
    return () => {
      invoke("stop_ring").catch(logIgnored);
    };
  }, [incomingCall, currentUser?.id, prefsQuery.data?.allow_sound_effects]);

  const pathname = useRouterState({ select: (s) => s.location.pathname });

  // The terminal stays mounted once visited (PTY + scrollback survive
  // navigation); the URL only governs visibility.
  const isTerminal = pathname === "/terminal";
  const [terminalActivated, setTerminalActivated] = useState(false);
  useEffect(() => {
    if (isTerminal) {
      setTerminalActivated(true);
    }
  }, [isTerminal]);

  // Global file drop: show the overlay and rebroadcast paths to the mounted
  // ChatInput via `pollis:pathdrop`.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    getCurrentWindow().onDragDropEvent((event) => {
      // No ChatInput mounted: no overlay. The bridge still preventDefaults, so
      // the window never navigates to the dropped file.
      if (!isDropTargetActive()) {
        setIsDragOver(false);
        return;
      }
      const dragging =
        event.payload.type === "enter" || event.payload.type === "over";
      // For pages with their own drop area (emoji upload), which the overlay
      // would cover.
      window.dispatchEvent(
        new CustomEvent("pollis:pathdragstate", { detail: { dragging } }),
      );
      if (dragging) {
        setIsDragOver(shouldShowDropOverlay());
      } else if (event.payload.type === "drop") {
        setIsDragOver(false);
        const paths = event.payload.paths;
        if (paths.length > 0) {
          window.dispatchEvent(new CustomEvent("pollis:pathdrop", { detail: { paths } }));
        }
      } else {
        setIsDragOver(false);
      }
    }).then((fn) => {
      // StrictMode: cleanup may already have run.
      if (cancelled) { fn(); } else { unlisten = fn; }
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Cold-launch MLS sweep: poll offline Welcomes and process pending commits
  // for every group + DM, so the first MLS action runs on caught-up state
  // (#371). `isSyncing` shows "Syncing…" and lets stateful actions defer.
  useEffect(() => {
    if (!currentUser) {
      return;
    }
    let cancelled = false;
    setIsSyncing(true);
    invoke('catch_up_all_mls_groups', { userId: currentUser.id })
      .catch((err) => {
        console.warn('[mls] catch_up_all_mls_groups failed:', err);
      })
      .finally(() => {
        if (!cancelled) {
          setIsSyncing(false);
          // Caught up: deliver what the durable outbox holds (#1335); the core
          // started this drain at unlock.
          void flushOutbox(queryClient);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [currentUser?.id]);

  // Network back: drain what failed while offline (#1335). Event, not timer.
  useEffect(() => {
    if (!currentUser) {
      return;
    }
    const onOnline = () => {
      void flushOutbox(queryClient);
    };
    window.addEventListener("online", onOnline);
    return () => {
      window.removeEventListener("online", onOnline);
    };
  }, [currentUser?.id, queryClient]);

  useEffect(() => {
    if (!currentUser) {
      return;
    }
    screenShareSession.ensureSubscribed().catch((err) => {
      console.warn('[screenshare] ensureSubscribed failed:', err);
    });
    cameraSession.ensureSubscribed().catch((err) => {
      console.warn('[camera] ensureSubscribed failed:', err);
    });
  }, [currentUser?.id]);

  // `groupsWithChannels` is a fresh ref every refetch; key the commit effect on
  // the membership shape instead.
  const membershipSignature = useMemo(() => {
    if (!groupsWithChannels) {
      return "";
    }
    return groupsWithChannels
      .map((g) => `${g.id}:${g.channels[0]?.id ?? ""}`)
      .sort()
      .join(",");
  }, [groupsWithChannels]);

  useEffect(() => {
    if (!currentUser || !groupsWithChannels) {
      return;
    }
    for (const group of groupsWithChannels) {
      const firstChannel = group.channels[0];
      if (!firstChannel) {
        continue;
      }
      invoke('process_pending_commits', { conversationId: firstChannel.id, userId: currentUser.id }).catch((err) => {
        console.warn(`[mls] process_pending_commits for group ${group.id}:`, err);
      });
    }
  }, [membershipSignature, currentUser?.id]);

  useLiveKitRealtime();

  // Warm the lazily-chunked call surfaces once idle (#874): answering a call
  // navigates to `/call/$callId` programmatically, where a chunk fetch is
  // unacceptable. Fire-and-forget; the route's lazy import retries.
  useEffect(() => {
    const warm = () => {
      void import("../../pages/Call").catch(logIgnored);
      void import("../../pages/VoiceChannel").catch(logIgnored);
    };
    if (typeof requestIdleCallback === "function") {
      const handle = requestIdleCallback(warm, { timeout: 5000 });
      return () => cancelIdleCallback(handle);
    }
    const handle = setTimeout(warm, 2000);
    return () => clearTimeout(handle);
  }, []);

  // Update checks while signed in (App.tsx covers launch); the module-singleton
  // poller can't be duplicated by remounts.
  useEffect(() => {
    if (import.meta.env.DEV) {
      return;
    }
    startUpdatePolling();
    return () => {
      stopUpdatePolling();
    };
  }, []);

  useBadge();

  // Idle auto-lock (#851): armed while the shell (the only thing showing
  // decrypted content) is mounted. Rust owns the deadline.
  useAutoLock(currentUser?.id ?? null);

  useEffect(() => {
    if (!groupsWithChannels) {
      return;
    }
    setGroups(groupsWithChannels);
    for (const g of groupsWithChannels) {
      setChannels(g.id, g.channels);
    }
  }, [groupsWithChannels, setGroups, setChannels]);

  const closeSearch = useCallback(() => setIsSearchOpen(false), []);
  // Stable identity: an inline arrow re-rendered the memoised Sidebar on every
  // shell render (#874).
  const toggleSidebar = useCallback(() => setIsSidebarOpen((v) => !v), []);

  // Dragged widths are pixels; clamp them when the window shrinks (#985).
  useEffect(() => {
    const clamp = () => panelWidthStore.clampToWindow(window.innerWidth);
    // Once on mount too: the window may have shrunk while closed.
    clamp();
    window.addEventListener("resize", clamp);
    return () => window.removeEventListener("resize", clamp);
  }, []);

  // BreadcrumbNav's search button fires this event.
  useEffect(() => {
    const handle = () => setIsSearchOpen(true);
    window.addEventListener("pollis:open-search", handle);
    return () => window.removeEventListener("pollis:open-search", handle);
  }, []);

  // onLock lets App.tsx flip appState to "pin-entry" (AppShell unmounts).
  const { onLock } = router.options.context as RouterContext;

  // Global shortcuts bind by command id; combos live in keyboard/commands.ts.

  useGlobalShortcut("app.toggleSidebar", () => {
    setIsSidebarOpen((v) => !v);
  });

  useGlobalShortcut("app.toggleRightPanel", () => {
    toggleRightPanel();
  });

  // Leaving uses history.back() so the prior chat view is restored exactly.
  useGlobalShortcut("app.toggleTerminal", () => {
    if (pathname === "/terminal") {
      router.history.back();
    } else {
      router.navigate({ to: "/terminal" });
    }
  });

  useGlobalShortcut("app.toggleSearch", () => {
    setIsSearchOpen((prev) => !prev);
  });

  useGlobalShortcut("app.lock", () => {
    onLock();
  });

  useGlobalShortcut("app.closeWindow", () => {
    hideWindow().catch(console.error);
  });

  // Mute/leave work from any page, only while in a call.
  useGlobalShortcut(
    "voice.toggleMute",
    () => {
      voiceSession
        .toggleMute()
        .catch((err) => console.error("[voice] toggleMute shortcut:", err));
    },
    { enabled: !!activeVoiceChannelId },
  );
  useGlobalShortcut(
    "voice.toggleDeafen",
    () => {
      voiceSession
        .toggleDeafen()
        .catch((err) => console.error("[voice] toggleDeafen shortcut:", err));
    },
    { enabled: !!activeVoiceChannelId },
  );

  // Push-to-talk is a HOLD command. The registry also fires `onRelease` on blur
  // or unmount mid-hold so the mic is never left open; `releasePushToTalk` has
  // Rust guarantee the same.
  useGlobalShortcut(
    "voice.pushToTalk",
    () => {
      voiceSession
        .setPushToTalkHeld(true)
        .catch((err) => console.error("[voice] pushToTalk down:", err));
    },
    {
      enabled: !!activeVoiceChannelId,
      onRelease: () => {
        voiceSession
          .releasePushToTalk()
          .catch((err) => console.error("[voice] pushToTalk up:", err));
      },
    },
  );

  useGlobalShortcut(
    "voice.leave",
    () => {
      voiceSession.leave();
    },
    { enabled: !!activeVoiceChannelId },
  );

  // Escape takes one history step back (disabled while search is open), through
  // the same guard as the breadcrumb's back chevron, so they never disagree.
  // No preventDefault, so capture-phase cancel-Esc handlers still work.
  useGlobalShortcut(
    "nav.back",
    () => {
      // Leave a fullscreen screen-share viewer before navigating.
      if (viewingScreenShareTrackKey !== null) {
        setViewingScreenShareTrackKey(null);
        return;
      }
      historyNavStore.goBack();
    },
    { enabled: !isSearchOpen, preventDefault: false },
  );

  // Refetch all queries without a reload, and sync MLS state.
  useGlobalShortcut("app.sync", () => {
    setIsSyncing(true);
    const mlsPromises: Promise<unknown>[] = [];
    if (currentUser) {
      mlsPromises.push(
        invoke("poll_mls_welcomes", { userId: currentUser.id }).catch(
          (err) => {
            console.warn("[mls] poll_mls_welcomes on sync:", err);
          },
        ),
      );
      for (const group of groupsWithChannels ?? []) {
        const firstChannel = group.channels[0];
        if (firstChannel) {
          mlsPromises.push(
            invoke("process_pending_commits", {
              conversationId: firstChannel.id,
              userId: currentUser.id,
            }).catch((err) => {
              console.warn(
                `[mls] process_pending_commits on sync for ${group.id}:`,
                err,
              );
            }),
          );
        }
      }
    }
    Promise.all([queryClient.invalidateQueries(), ...mlsPromises]).finally(() =>
      setIsSyncing(false),
    );
  });

  useEffect(() => {
    const handleWindowFocus = () => {
      if (!document.activeElement || document.activeElement === document.body) {
        const menu = document.querySelector<HTMLElement>('[role="menu"]');
        if (menu) {
          menu.focus();
          return;
        }
        const input = document.querySelector<HTMLElement>(
          'input:not([type="hidden"]), textarea'
        );
        input?.focus();
      }
    };
    window.addEventListener("focus", handleWindowFocus);
    return () => window.removeEventListener("focus", handleWindowFocus);
  }, []);

  // Clear the status bar alert on reaching the room that triggered it.
  useEffect(() => {
    if (statusBarAlert && pathname.includes(statusBarAlert.roomId)) {
      setStatusBarAlert(null);
    }
  }, [pathname, statusBarAlert, setStatusBarAlert]);

  const isChatScreen = useMemo(() => {
    const channelMatch = pathname.match(/^\/groups\/[^/]+\/channels\/([^/]+)/);
    if (channelMatch && channelMatch[1] !== "new") {
      return true;
    }
    const dmMatch = pathname.match(/^\/dms\/([^/]+)/);
    if (dmMatch && dmMatch[1] !== "new") {
      return true;
    }
    return false;
  }, [pathname]);

  // Refined keeps the bottom bar neutral; terminal fills it with the accent.
  // `barInk` stays legible on `barBg`.
  const refined = skin === "refined";
  const barBg = refined
    ? "var(--c-surface)"
    : isChatScreen ? "var(--c-bg)" : "var(--c-accent)";
  const barInk = refined
    ? "var(--c-text-dim)"
    : isChatScreen ? "var(--c-accent)" : "var(--c-surface)";

  const voiceChannelName = useMemo(() => {
    if (!activeVoiceChannelId) {
      return t("shell.voiceChannelFallback");
    }
    if (activeVoiceChannelId.startsWith("call-")) {
      const peerId =
        voiceState.kind === "joined" || voiceState.kind === "joining"
          ? voiceState.counterpartyUserId
          : null;
      if (peerId) {
        const peer = voiceParticipants.find(
          (p) => userIdFromVoiceIdentity(p.identity) === peerId,
        );
        if (peer?.name) {
          return peer.name;
        }
          // Peer not in the room yet (outgoing ring): use the incoming-call name.
        if (incomingCall && incomingCall.callerId === peerId) {
          return incomingCall.callerUsername;
        }
      }
      return t("shell.callChannel");
    }
    for (const g of groupsWithChannels ?? []) {
      const ch = g.channels.find((c) => c.id === activeVoiceChannelId);
      if (ch) {
        return ch.name;
      }
    }
    return t("shell.voiceChannelFallback");
  }, [activeVoiceChannelId, groupsWithChannels, voiceState, voiceParticipants, incomingCall, t]);

  return (
    <div
      data-testid="terminal-app"
      className="bg-bg"
      style={{
        height: "100%",
        width: "100%",
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
        position: "relative",
      }}
    >
      <WindowResizeEdges />

      <SearchPanel isOpen={isSearchOpen} onClose={closeSearch} />

      <TitleBar />

      <BreadcrumbNav />

        {/* The screen-share viewer mounts inside this region so the chrome stays usable. */}
      <div style={{ flex: 1, overflow: "hidden", display: "flex", flexDirection: "row", position: "relative" }}>
        <Sidebar isOpen={isSidebarOpen} onToggle={toggleSidebar} />
        <div
          style={{
            flex: 1,
            overflow: "hidden",
            display: isTerminal ? "none" : "flex",
            flexDirection: "column",
            minWidth: 0,
            position: "relative",
          }}
        >
          <Outlet />
          <ScreenShareViewer />
        </div>
          {/* #824: a flex sibling, so opening it reflows the list rather than covering it. */}
        {!isTerminal && <RightPanel />}
        {terminalActivated && (
          <div
            style={{
              flex: 1,
              overflow: "hidden",
              display: isTerminal ? "flex" : "none",
              flexDirection: "column",
              minWidth: 0,
            }}
          >
            <Suspense fallback={null}>
              <TerminalView visible={isTerminal} />
            </Suspense>
          </div>
        )}
      </div>

      {/* Only once joined: the controls need a connected room. */}
      {voiceState.kind === 'joined' && skin === 'terminal' && (
        <VoiceBar
          channelId={voiceState.channelId}
          channelName={voiceChannelName}
        />
      )}

      {isDragOver && (
        <div
          className="absolute inset-0 flex items-center justify-center pointer-events-none"
          style={{ zIndex: 9000, background: "rgba(0,0,0,0.7)" }}
        >
          <div
            className="flex flex-col items-center gap-2"
            style={{
              border: "2px dashed var(--c-accent)",
              borderRadius: 8,
              padding: "28px 56px",
            }}
          >
            <span className="text-sm font-mono text-accent">
              {t("shell.dropFiles")}
            </span>
          </div>
        </div>
      )}

      <div
        className="border-t border-line"
        style={{
          flexShrink: 0,
          background: barBg,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "8px 10px",
        }}
      >
        <StatusBarSummary color={barInk} />
        <div className="flex items-center gap-3">
        {availableUpdateVersion && (
          <PressButton
            data-testid="status-bar-update-available"
            className="text-xs font-mono flex items-center gap-1 cursor-pointer border-0"
            style={{
              color: barInk,
              background: "none",
              padding: 0,
              lineHeight: 0,
            }}
            onClick={() => router.navigate({ to: "/update" })}
            aria-label={t("statusBar.updateAvailableTitle", { version: availableUpdateVersion })}
            title={t("statusBar.updateAvailableTitle", { version: availableUpdateVersion })}
          >
            <Download className="w-4 h-4" />
            <span>{t("statusBar.updateAvailable")}</span>
          </PressButton>
        )}
        {/* Always rendered at fixed height so the bar doesn't reflow. */}
        <div className="flex items-center justify-end h-4 leading-none">
        {incomingCall ? (
          <div
            data-testid="status-bar-incoming-call"
            className="flex items-center gap-2"
            style={{ color: barInk }}
          >
            <button
              data-testid="status-bar-incoming-call-accept"
              className="text-xs font-mono status-bar-blink flex items-center gap-1 cursor-pointer border-0"
              style={{ color: "inherit", background: "none", padding: 0 }}
              onClick={() => {
                // Order matters: route first so the old voice page unmounts
                // before activeVoiceChannelId flips, then swap rooms, then
                // clear the alert.
                const callId = incomingCall.callId;
                router.navigate({ to: "/call/$callId", params: { callId } });
                voiceSession.setIntent({
                  channelId: incomingCall.roomName,
                  groupId: null,
                  counterpartyUserId: incomingCall.callerId,
                });
                setIncomingCall(null);
                // Stop ringing on this user's other devices; our own
                // `call_canceled` handler is idempotent.
                if (currentUser) {
                  invoke("dismiss_call_on_my_devices", {
                    userId: currentUser.id,
                    callId,
                  }).catch((e) => console.warn("dismiss_call_on_my_devices failed", e));
                }
              }}
              aria-label={t("statusBar.answerCall", { username: incomingCall.callerUsername })}
            >
              <Phone className="w-4 h-4" />: <bdi>@{incomingCall.callerUsername}</bdi>
            </button>
            <button
              data-testid="status-bar-incoming-call-decline"
              className="cursor-pointer border-0"
              style={{ color: "inherit", background: "none", padding: 0, lineHeight: 0 }}
              onClick={() => {
                const callerId = incomingCall.callerId;
                const callId = incomingCall.callId;
                setIncomingCall(null);
                invoke("cancel_call", { otherUserId: callerId, callId }).catch((e) => console.warn("cancel_call failed", e));
                // Stop ringing on this user's other devices.
                if (currentUser) {
                  invoke("dismiss_call_on_my_devices", {
                    userId: currentUser.id,
                    callId,
                  }).catch((e) => console.warn("dismiss_call_on_my_devices failed", e));
                }
              }}
              aria-label={t("statusBar.declineCall")}
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        ) : voiceError ? (
          <div
            data-testid="status-bar-voice-error"
            className="flex items-center gap-2"
            style={{ color: barInk }}
          >
            <span className="text-xs font-mono flex items-center gap-1">
              <AlertTriangle className="w-4 h-4" />
              {voiceError}
            </span>
            <button
              data-testid="status-bar-voice-error-dismiss"
              className="cursor-pointer border-0"
              style={{ color: "inherit", background: "none", padding: 0, lineHeight: 0 }}
              onClick={() => setVoiceError(null)}
              aria-label={t("statusBar.dismissVoiceError")}
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        ) : screenShareError ? (
          <div
            data-testid="status-bar-screenshare-error"
            className="flex items-center gap-2 min-w-0 flex-1"
            style={{ color: barInk }}
          >
            <span
              className="text-xs font-mono flex items-center gap-1 min-w-0 truncate"
              title={screenShareError}
            >
              <AlertTriangle className="w-4 h-4 flex-shrink-0" />
              <span className="truncate">{screenShareError}</span>
            </span>
            <button
              data-testid="status-bar-screenshare-error-dismiss"
              className="cursor-pointer border-0"
              style={{ color: "inherit", background: "none", padding: 0, lineHeight: 0 }}
              onClick={() => shareStopped()}
              aria-label={t("statusBar.dismissScreenShareError")}
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        ) : statusBarAlert ? (
          <button
            className="text-xs font-mono status-bar-blink flex items-center gap-1 cursor-pointer border-0"
            style={{ color: barInk, background: "none", padding: 0 }}
            onClick={() => {
              router.navigate({ to: "/dms/$conversationId", params: { conversationId: statusBarAlert.roomId } });
              setStatusBarAlert(null);
            }}
          >
            <Mail className="w-4 h-4" />: <bdi>@{statusBarAlert.senderUsername}</bdi>
          </button>
        ) : isSyncing ? (
          <div
            data-testid="status-bar-syncing"
            className="flex items-center gap-1.5 text-xs font-mono pointer-events-none"
            style={{ color: barInk }}
          >
            <span>{t("statusBar.syncing")}</span>
            <LoadingSpinner size="sm" />
          </div>
        ) : null}
        </div>
        </div>
      </div>
    </div>
  );
});
