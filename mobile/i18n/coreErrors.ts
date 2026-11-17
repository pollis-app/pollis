// pollis-core reports PIN and lock failures as English text (pollis-core/src/commands/pin.rs,
// device_unlock.rs), and screens show a failure's message as-is. This maps those messages onto
// `errors:core.*` (#1724), and so does a refusal from the Delivery Service, which the core reports
// as `<label> <path> <status>: <body>` (`DsRefused` in pollis-core/src/commands/mls/ds_client.rs).
// Anything it does not recognise is returned unchanged. Applied once, at
// the bridge (frontend/src/bridge/invoke.ts, mobile/lib/native/bridge.ts), after the
// device_revoked / upgrade_required checks have seen the raw text.
// mobile/i18n/coreErrors.ts is a copy; mobile/tests/core-errors.test.ts keeps the two identical
// and pins the Rust strings, so a reworded core message fails a test instead of going English.
import i18n from "i18next";

/** The localized text for a core error message, or the message itself when it is not one we map. */
export function localizeCoreError(message: string): string {
  const incorrect = /^pin incorrect; (\d+) attempts remaining$/.exec(message);
  if (incorrect) {
    const count = Number(incorrect[1]);
    // The last wrong attempt has already reset the PIN: say what to do next, not "0 left".
    if (count === 0) {
      return i18n.t("errors:core.pinLockedOut");
    }
    return i18n.t("errors:core.pinIncorrect", { count });
  }
  if (message.startsWith("pin locked out")) {
    return i18n.t("errors:core.pinLockedOut");
  }
  if (message.startsWith("PIN not set for user")) {
    return i18n.t("errors:core.pinNotSet");
  }
  if (message.startsWith("no active user")) {
    return i18n.t("errors:core.noActiveUser");
  }
  if (message === "unlock Pollis first") {
    return i18n.t("errors:core.unlockFirst");
  }
  const refused = SERVER_REFUSAL.exec(message);
  if (refused) {
    return localizeServerRefusal(Number(refused[1]), refused[2]);
  }
  return message;
}

// `ds_post /v1/groups/create 401 Unauthorized: {"error":"unauthorized"}`, possibly after a prefix.
const SERVER_REFUSAL = /(?:^|: )ds(?:_[a-z_]+)? \/v1\/\S+ (\d{3})[^:]*: ?([\s\S]*)$/;

function localizeServerRefusal(status: number, body: string): string {
  // Signed requests are refused when the device's clock is far off, the usual cause on a desktop.
  if (status === 401 || status === 403) {
    return i18n.t("errors:core.serverUnverified");
  }
  if (status === 429) {
    return i18n.t("errors:core.serverRateLimited");
  }
  if (status >= 500) {
    return i18n.t("errors:core.serverUnavailable");
  }
  return serverSentence(body) ?? i18n.t("errors:core.serverRefused");
}

// The DS's own `{"error": "…"}` when it is a sentence ("email already in use"), not a code.
function serverSentence(body: string): string | null {
  let text: unknown;
  try {
    text = (JSON.parse(body) as { error?: unknown } | null)?.error;
  } catch {
    return null;
  }
  if (typeof text !== "string" || !text.trim().includes(" ")) {
    return null;
  }
  const sentence = text.trim();
  const capitalised = sentence.charAt(0).toUpperCase() + sentence.slice(1);
  return /[.!?]$/.test(capitalised) ? capitalised : `${capitalised}.`;
}

/**
 * A rejected command with a mapped message swapped for its localized text: a string for a
 * string, a new Error for an Error. Anything unmapped is returned as the same value.
 */
export function localizeCoreFailure(error: unknown): unknown {
  if (typeof error === "string") {
    return localizeCoreError(error);
  }
  if (error instanceof Error) {
    const text = localizeCoreError(error.message);
    return text === error.message ? error : new Error(text);
  }
  return error;
}

/** True when the core refused because the PIN is locked out (the message, before or after mapping). */
export function isPinLockedOut(message: string): boolean {
  return (
    message.startsWith("pin locked out") ||
    message === "pin incorrect; 0 attempts remaining" ||
    message === i18n.t("errors:core.pinLockedOut")
  );
}
