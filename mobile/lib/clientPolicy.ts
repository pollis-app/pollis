// The minimum-version check on mobile (#1611). Pure: the rule, the cache and the throttle live in
// pollis-core (`check_client_policy`); this holds the verdict the root layout renders.

import type { InstallChannel, UpdateDestinationKind } from "./installChannel.ts";

export type ClientPlatform = "ios" | "android-play" | "android-sideload";

/** Mirrors `pollis_core::client_policy::ClientPolicyStatus`. */
export interface ClientPolicyStatus {
  blocked: boolean;
  platform: string | null;
  version: string | null;
  minimum: string | null;
  message: string | null;
  source: "network" | "cache" | "ds" | "none";
}

/** This build's policy key. Play vs sideload comes from the baked install channel (#1443). */
export function clientPlatformFor(os: "ios" | "android", channel: InstallChannel | null): ClientPlatform {
  if (os === "ios") {
    return "ios";
  }
  return channel === "store" ? "android-play" : "android-sideload";
}

export function isUpgradeRequiredError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // Exactly `upgrade_required`, or appended after ": "; a mere mention is not it.
  return message === "upgrade_required" || message.endsWith(": upgrade_required");
}

/** `nav:clientBlocked.*` key for the button that opens where this install updates from. */
export function openLabelKey(kind: UpdateDestinationKind): string {
  switch (kind) {
    case "appstore":
      return "clientBlocked.openAppStore";
    case "play":
      return "clientBlocked.openPlay";
    case "testflight":
      return "clientBlocked.openTestflight";
    case "web":
      return "clientBlocked.openWebsite";
  }
}

/** The versions line: with a minimum when one is known. */
export function versionsLine(status: ClientPolicyStatus): { key: string; vars: Record<string, string> } | null {
  if (!status.version) {
    return null;
  }
  if (status.minimum) {
    return { key: "clientBlocked.versions", vars: { version: status.version, minimum: status.minimum } };
  }
  return { key: "clientBlocked.installed", vars: { version: status.version } };
}

/** The manifest's message replaces the built-in body when set. */
export function bodyText(status: ClientPolicyStatus, builtIn: string): string {
  const m = status.message?.trim();
  return m ? m : builtIn;
}

// --- The verdict store ---

let current: ClientPolicyStatus | null = null;
const listeners = new Set<() => void>();
let detailsHandler: (() => void | Promise<void>) | null = null;
let detailsRunning = false;

export function getClientPolicy(): ClientPolicyStatus | null {
  return current;
}

export function isBlocked(): boolean {
  return current?.blocked === true;
}

export function setClientPolicy(next: ClientPolicyStatus | null): void {
  current = next;
  for (const l of listeners) {
    l();
  }
}

export function subscribeClientPolicy(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** What to run when a command fails with `upgrade_required` (fetch the details). */
export function onUpgradeRequired(next: () => void | Promise<void>): () => void {
  detailsHandler = next;
  return () => {
    if (detailsHandler === next) {
      detailsHandler = null;
    }
  };
}

/**
 * Called for every bridge failure. An `upgrade_required` only asks the core; the store blocks
 * only when the core's own verdict says so (`applyCoreVerdict`), so a late or stray rejection
 * after an unblock never re-blocks.
 */
export function noticeUpgradeRequired(error: unknown): void {
  if (!isUpgradeRequiredError(error) || !detailsHandler || detailsRunning) {
    return;
  }
  detailsRunning = true;
  const run = detailsHandler;
  queueMicrotask(() => {
    void Promise.resolve()
      .then(run)
      .catch((e) => console.warn("[client-policy] details failed", e))
      .finally(() => {
        detailsRunning = false;
      });
  });
}

/** The core's answer after an `upgrade_required`: only `blocked: true` blocks; anything else is ignored. */
export function applyCoreVerdict(status: ClientPolicyStatus | null | undefined): void {
  if (status?.blocked === true) {
    setClientPolicy(status);
  }
}

/**
 * Launch: a cache that already allows this build lets the app go on while a fresh check runs in
 * the background; a cached block, or no cache, waits for the check (bounded in the core).
 */
export function launchWaitsForCheck(cached: ClientPolicyStatus | null | undefined): boolean {
  return !(cached && !cached.blocked && cached.source === "cache");
}

// --- Dev override links (Maestro): pollis://dev/client-policy?policy=…|url=…|clear=1 ---

export type DevPolicyLink =
  | { kind: "set"; policy?: string; url?: string }
  | { kind: "clear" };

export function parseDevPolicyLink(link: string): DevPolicyLink | null {
  const m = /^pollis:\/\/\/?dev\/client-policy\/?(?:\?(.*))?$/.exec(link.trim());
  if (!m) {
    return null;
  }
  const params = new Map<string, string>();
  for (const pair of (m[1] ?? "").split("&")) {
    if (!pair) {
      continue;
    }
    const i = pair.indexOf("=");
    const k = i < 0 ? pair : pair.slice(0, i);
    const v = i < 0 ? "" : pair.slice(i + 1);
    try {
      params.set(decodeURIComponent(k), decodeURIComponent(v.replace(/\+/g, " ")));
    } catch {
      return null;
    }
  }
  if (params.has("clear")) {
    return { kind: "clear" };
  }
  const policy = params.get("policy");
  const url = params.get("url");
  if (policy) {
    return { kind: "set", policy };
  }
  if (url) {
    return { kind: "set", url };
  }
  return null;
}
