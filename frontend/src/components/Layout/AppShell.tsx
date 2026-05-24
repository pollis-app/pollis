import React, { useEffect, useState, useMemo, useCallback, useRef } from "react";
import { Outlet, useRouter, useRouterState } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { invoke, getCurrentWindow, hideWindow } from "../../bridge";
import { TitleBar } from "./TitleBar";
import { BreadcrumbNav } from "./BreadcrumbNav";
import { Sidebar } from "./Sidebar";
import { StatusBarSummary } from "./StatusBarSummary";
import { VoiceBar } from "../Voice/VoiceBar";
import { ScreenShareViewer } from "../Voice/ScreenShareViewer";
import { screenShareSession } from "../../screenshare/screenShareSession";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { SearchPanel } from "../SearchPanel";
import { TerminalView } from "../TerminalView";
import { useAppStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useLiveKitRealtime } from "../../hooks/useLiveKitRealtime";
import { useBadge } from "../../hooks/useBadge";
import { AlertTriangle, Mail, Phone, X } from "lucide-react";
import { loadDeviceCallRingtone } from "../../utils/notify";
import { usePreferences } from "../../hooks/queries/usePreferences";
import { voiceSession } from "../../voice";
import { useGlobalShortcut } from "../../keyboard";
import type { RouterContext } from "../../types/router";

/**
 * AppShell is the root route component rendered by RouterProvider.
 * It owns the terminal chrome (TitleBar, BreadcrumbNav, VoiceBar, bottom status bar)
 * and renders the matched child route via <Outlet />.
 */
export const AppShell: React.FC = () => {
  const [isSyncing, setIsSyncing] = useState(false);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const [isDragOver, setIsDragOver] = useState(false);
  // Sidebar visibility is driven by the user preference
  // `sidebar_open_by_default`. Cmd/Ctrl+B toggles for the current session
  // only — flipping it does NOT write back to the preference, so users
  // who keep it closed by default can still pop it open ad-hoc without
  // losing their default. Changing the preference updates the live UI
  // via the sync effect below.
  const [isSidebarOpen, setIsSidebarOpen] = useState<boolean>(true);
  const queryClient = useQueryClient();
  const router = useRouter();

  const {
    setGroups,
    setChannels,
    activeVoiceChannelId,
    statusBarAlert,
    setStatusBarAlert,
    voiceError,
    setVoiceError,
    screenShareError,
    setScreenShareError,
    isLocalSpeaking,
    incomingCall,
    setIncomingCall,
    viewingScreenShareTrackKey,
    setViewingScreenShareTrackKey,
  } = useAppStore();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { query: prefsQuery } = usePreferences();

  // Apply the sidebar default ONCE per session, the first time prefs land.
  // The preference governs only the initial open/closed state at app
  // start — toggling it later in Preferences must not slam the live
  // sidebar shut or open, since that would clobber the user's current
  // Cmd/Ctrl+B state (or their click on the sidebar's collapse handle).
  const sidebarDefault = prefsQuery.data?.sidebar_open_by_default;
  const sidebarDefaultApplied = useRef(false);
  useEffect(() => {
    if (sidebarDefault !== undefined && !sidebarDefaultApplied.current) {
      setIsSidebarOpen(sidebarDefault);
      sidebarDefaultApplied.current = true;
    }
  }, [sidebarDefault]);

  const currentUser = useAppStore((s) => s.currentUser);

  // Drive the looping ringtone off the incomingCall slot. Rust owns the
  // playback thread (`start_ring` / `stop_ring`) so the loop survives any
  // re-render churn here. Both the device-local ringtone toggle and the
  // account-level allow_sound_effects must be on for the ring to play; OS
  // notification (a single ping on arrival) is still fired from notify.ts.
  useEffect(() => {
    if (!incomingCall) {
      invoke("stop_ring").catch(() => {});
      return;
    }
    const allowGlobal = prefsQuery.data?.allow_sound_effects ?? true;
    const allowDevice = loadDeviceCallRingtone(currentUser?.id ?? null);
    if (allowGlobal && allowDevice) {
      invoke("start_ring").catch(() => {});
    }
    return () => {
      invoke("stop_ring").catch(() => {});
    };
  }, [incomingCall, currentUser?.id, prefsQuery.data?.allow_sound_effects]);

  // ─── Current route pathname — needed by keyboard handlers below ─────────────
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  // The terminal is a persistent component (mounted lazily on first
  // visit, then kept mounted and display-toggled) so the PTY session +
  // scrollback survive navigation. The URL only governs visibility:
  // clicking a status-bar link / Cmd+K result / Back moves off
  // /terminal like any other view, with zero terminal-specific wiring.
  const isTerminal = pathname === "/terminal";
  const [terminalActivated, setTerminalActivated] = useState(false);
  useEffect(() => {
    if (isTerminal) {
      setTerminalActivated(true);
    }
  }, [isTerminal]);

  // Global file drop — Tauri intercepts OS drag-drop before the browser sees it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") {
        setIsDragOver(true);
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
      // If cleanup already ran (React StrictMode double-invoke), unlisten immediately.
      if (cancelled) { fn(); } else { unlisten = fn; }
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // On startup, apply any MLS Welcome messages that arrived while offline.
  useEffect(() => {
    if (!currentUser) {
      return;
    }
    invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
      console.warn('[mls] poll_mls_welcomes failed:', err);
    });
  }, [currentUser?.id]);

  // Once authenticated, hook up the screen-share event + frame Channels.
  // Idempotent — only the first call actually invokes the backend.
  useEffect(() => {
    if (!currentUser) {
      return;
    }
    screenShareSession.ensureSubscribed().catch((err) => {
      console.warn('[screenshare] ensureSubscribed failed:', err);
    });
  }, [currentUser?.id]);

  // When group membership changes (someone joins/leaves while we're online),
  // process any pending MLS commits so our epoch stays current.
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
  }, [groupsWithChannels]);

  // Maintain a LiveKit room connection for the active channel/conversation
  useLiveKitRealtime();

  // Sync unread count to OS dock/taskbar badge
  useBadge();

  // Sync groups+channels into the store once loaded
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

  // The search button in BreadcrumbNav fires this custom event so it can
  // open the panel without lifting AppShell's local state into a store.
  useEffect(() => {
    const handle = () => setIsSearchOpen(true);
    window.addEventListener("pollis:open-search", handle);
    return () => window.removeEventListener("pollis:open-search", handle);
  }, []);

  // Routes through the router context's onLock so App.tsx can flip the
  // top-level appState to "pin-entry" (AppShell unmounts in the process).
  const { onLock } = router.options.context as RouterContext;

  // ─── Global keyboard commands ───────────────────────────────────────────────
  // Bound by stable command id; the key combo lives in keyboard/commands.ts
  // (and, in future, a user-override map) — never named here. One shared
  // dispatcher replaces the former per-shortcut window listeners. Scoped
  // element onKeyDown handlers (chat input, grids, OTP, …) are intentionally
  // left as-is.

  useGlobalShortcut("app.toggleSidebar", () => {
    setIsSidebarOpen((v) => !v);
  });

  // Leaving uses history.back() so the prior chat view (and its selected
  // channel) is restored exactly.
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

  // Hide the window on macOS, close it on Windows/Linux.
  useGlobalShortcut("app.closeWindow", () => {
    hideWindow().catch(console.error);
  });

  // Discord users reach for the keyboard for mute/leave (not tile
  // traversal), so these work from any page — active only while in a call.
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
    "voice.leave",
    () => {
      voiceSession.leave();
    },
    { enabled: !!activeVoiceChannelId },
  );

  // Navigate back in history (disabled while the search panel is open). If
  // currently viewing a channel, go directly to the group page to avoid
  // landing on "create channel" if that was in history. preventDefault off
  // to mirror the prior behavior and stay out of the way of the
  // capture-phase modal-cancel Esc handlers.
  useGlobalShortcut(
    "nav.back",
    () => {
      // Exit fullscreen screen-share viewer first if active, so escape
      // backs out of the viewer before navigating history.
      if (viewingScreenShareTrackKey !== null) {
        setViewingScreenShareTrackKey(null);
        return;
      }
      const channelMatch = pathname.match(
        /^\/groups\/([^/]+)\/channels\/([^/]+)/,
      );
      if (channelMatch && channelMatch[2] !== "new") {
        router.navigate({
          to: "/groups/$groupId",
          params: { groupId: channelMatch[1] },
        });
      } else {
        router.history.back();
      }
    },
    { enabled: !isSearchOpen, preventDefault: false },
  );

  // Refetch all queries without a page reload, also sync MLS state.
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

  // Auto-focus when the window gains focus (e.g. switching back from another app)
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

  // Clear the status bar alert when the user navigates to the room that
  // triggered it.
  useEffect(() => {
    if (statusBarAlert && pathname.includes(statusBarAlert.roomId)) {
      setStatusBarAlert(null);
    }
  }, [pathname, statusBarAlert, setStatusBarAlert]);

  // Chat screens: channel view or DM conversation (not /new)
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

  // Find the voice channel name for the VoiceBar
  const voiceChannelName = useMemo(() => {
    if (!activeVoiceChannelId) {
      return "voice";
    }
    if (activeVoiceChannelId.startsWith("call-")) {
      return "call";
    }
    for (const g of groupsWithChannels ?? []) {
      const ch = g.channels.find((c) => c.id === activeVoiceChannelId);
      if (ch) {
        return ch.name;
      }
    }
    return "voice";
  }, [activeVoiceChannelId, groupsWithChannels]);

  return (
    <div
      data-testid="terminal-app"
      style={{
        height: "100%",
        width: "100%",
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
        background: "var(--c-bg)",
        position: "relative",
      }}
    >
      {/* Cmd/Ctrl+K search panel */}
      <SearchPanel isOpen={isSearchOpen} onClose={closeSearch} />

      {/* Title bar */}
      <TitleBar />

      {/* Breadcrumb nav — appears on every authenticated page */}
      <BreadcrumbNav />

      {/* Main content — sidebar + matched child route. The screen-share
          viewer mounts INSIDE this region so the TitleBar (drag handle),
          BreadcrumbNav, VoiceBar, and bottom status bar all stay visible
          and interactive while a stream is being viewed. */}
      <div style={{ flex: 1, overflow: "hidden", display: "flex", flexDirection: "row", position: "relative" }}>
        <Sidebar isOpen={isSidebarOpen} onToggle={() => setIsSidebarOpen((v) => !v)} />
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
            <TerminalView visible={isTerminal} />
          </div>
        )}
      </div>

      {/* VoiceBar — shown above bottom bar while user is in a voice channel */}
      {activeVoiceChannelId !== null && (
        <VoiceBar
          channelId={activeVoiceChannelId}
          channelName={voiceChannelName}
        />
      )}

      {/* Drag-over overlay */}
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
            <span className="text-sm font-mono" style={{ color: "var(--c-accent)" }}>
              drop files to send
            </span>
          </div>
        </div>
      )}

      {/* Bottom bar — unread summary on the left, status alert on the right */}
      {/* On chat screens, invert: dark bg with accent text. Otherwise: accent bg with dark text. */}
      <div
        style={{
          flexShrink: 0,
          borderTop: "1px solid var(--c-border)",
          background: isChatScreen ? "var(--c-bg)" : "var(--c-accent)",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "8px 10px",
        }}
      >
        <StatusBarSummary color={isChatScreen ? "var(--c-accent)" : "black"} />
        {/* Fixed-height, always-rendered slot so the bar doesn't reflow as
            the status (incoming call / alert / syncing) appears and clears. */}
        <div className="flex items-center justify-end h-4 leading-none">
        {incomingCall ? (
          <div
            data-testid="status-bar-incoming-call"
            className="flex items-center gap-2"
            style={{ color: isChatScreen ? "var(--c-accent)" : "var(--c-surface)" }}
          >
            <button
              data-testid="status-bar-incoming-call-accept"
              className="text-xs font-mono status-bar-blink flex items-center gap-1 cursor-pointer"
              style={{ color: "inherit", background: "none", border: "none", padding: 0 }}
              onClick={() => {
                // Order matters: route first (so the old voice page unmounts
                // before activeVoiceChannelId flips and any in-flight Call
                // page useEffect tries to bounce off a transient mismatch),
                // then swap the voice room, then clear the alert.
                router.navigate({ to: "/call/$callId", params: { callId: incomingCall.callId } });
                voiceSession.setIntent({
                  channelId: incomingCall.roomName,
                  groupId: null,
                  counterpartyUserId: incomingCall.callerId,
                });
                setIncomingCall(null);
              }}
              aria-label={`Answer call from @${incomingCall.callerUsername}`}
            >
              <Phone className="w-4 h-4" />: @{incomingCall.callerUsername}
            </button>
            <button
              data-testid="status-bar-incoming-call-decline"
              className="cursor-pointer"
              style={{ color: "inherit", background: "none", border: "none", padding: 0, lineHeight: 0 }}
              onClick={() => {
                const callerId = incomingCall.callerId;
                const callId = incomingCall.callId;
                setIncomingCall(null);
                invoke("cancel_call", { otherUserId: callerId, callId }).catch(() => {});
              }}
              aria-label="Decline call"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        ) : voiceError ? (
          <div
            data-testid="status-bar-voice-error"
            className="flex items-center gap-2"
            style={{ color: isChatScreen ? "var(--c-accent)" : "var(--c-surface)" }}
          >
            <span className="text-xs font-mono flex items-center gap-1">
              <AlertTriangle className="w-4 h-4" />
              {voiceError}
            </span>
            <button
              data-testid="status-bar-voice-error-dismiss"
              className="cursor-pointer"
              style={{ color: "inherit", background: "none", border: "none", padding: 0, lineHeight: 0 }}
              onClick={() => setVoiceError(null)}
              aria-label="Dismiss voice error"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        ) : screenShareError ? (
          <div
            data-testid="status-bar-screenshare-error"
            className="flex items-center gap-2 min-w-0 flex-1"
            style={{ color: isChatScreen ? "var(--c-accent)" : "var(--c-surface)" }}
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
              className="cursor-pointer"
              style={{ color: "inherit", background: "none", border: "none", padding: 0, lineHeight: 0 }}
              onClick={() => setScreenShareError(null)}
              aria-label="Dismiss screen share error"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        ) : statusBarAlert ? (
          <button
            className="text-xs font-mono status-bar-blink flex items-center gap-1 cursor-pointer"
            style={{ color: isChatScreen ? "var(--c-accent)" : "var(--c-surface)", background: "none", border: "none", padding: 0 }}
            onClick={() => {
              router.navigate({ to: "/dms/$conversationId", params: { conversationId: statusBarAlert.roomId } });
              setStatusBarAlert(null);
            }}
          >
            <Mail className="w-4 h-4" />: @{statusBarAlert.senderUsername}
          </button>
        ) : isSyncing ? (
          <div
            data-testid="status-bar-syncing"
            className="flex items-center gap-1.5 text-xs font-mono pointer-events-none"
            style={{ color: isChatScreen ? "var(--c-accent)" : "var(--c-surface)" }}
          >
            <span>syncing…</span>
            <LoadingSpinner size="sm" />
          </div>
        ) : null}
        </div>
      </div>
    </div>
  );
};
