import type { TFunction } from "i18next";
import type { ActionKind, ManifestChange, TriggerKind } from "../../types";
import { activeLocale, formatFileSize } from "../../utils/format";
import { errorMessage } from "../../utils/errorMessage";
import { appApprovalErrorKind, isAppReasonCode, type AppLine } from "../../utils/botApps";

type T = TFunction<"channels">;

/** "a, b and c" in the app language. */
function joinList(items: ReadonlyArray<string>): string {
  try {
    return new Intl.ListFormat(activeLocale(), { style: "long", type: "conjunction" }).format(items);
  } catch {
    return items.join(", ");
  }
}

/** "30 days" in the app language, plural rules and all. */
function formatDays(days: number): string {
  try {
    return new Intl.NumberFormat(activeLocale(), { style: "unit", unit: "day", unitDisplay: "long" }).format(days);
  } catch {
    return String(days);
  }
}

/** A schedule's period in the largest whole unit: "10 minutes", "1 hour", "45 seconds". */
function formatPeriod(seconds: number): string {
  const [n, unit] =
    seconds > 0 && seconds % 86400 === 0
      ? [seconds / 86400, "day"]
      : seconds > 0 && seconds % 3600 === 0
        ? [seconds / 3600, "hour"]
        : seconds > 0 && seconds % 60 === 0
          ? [seconds / 60, "minute"]
          : [seconds, "second"];
  try {
    return new Intl.NumberFormat(activeLocale(), { style: "unit", unit, unitDisplay: "long" }).format(n);
  } catch {
    return `${seconds}s`;
  }
}

function formatBytes(bytes: number): string {
  return bytes === 0 ? "0" : formatFileSize(bytes);
}

function channelLabel(t: T, selector: string): string {
  if (selector === "*") {
    return t("channels:bots.app.anyChannel");
  }
  return selector.startsWith("#") ? selector : `#${selector}`;
}

function triggerLabel(t: T, kind: string): string {
  return t(`channels:bots.app.triggers.${kind as TriggerKind}`, { defaultValue: kind });
}

function actionLabel(t: T, kind: string): string {
  return t(`channels:bots.app.actions.${kind as ActionKind}`, { defaultValue: kind });
}

/** One line of "What it can do". */
export function appLineText(t: T, line: AppLine): string {
  switch (line.kind) {
    case "reads": {
      const values = {
        events: line.edited ? t("channels:bots.app.lines.eventsNewEdited") : t("channels:bots.app.lines.eventsNew"),
        channels: joinList(line.channels.map((c) => channelLabel(t, c))),
        history: formatDays(line.history),
      };
      return line.history > 0 ? t("channels:bots.app.lines.reads", values) : t("channels:bots.app.lines.readsNoHistory", values);
    }
    case "readsNothing":
      return t("channels:bots.app.lines.readsNothing");
    case "wakes":
      return t("channels:bots.app.lines.wakes", { triggers: joinList(line.triggers.map((k) => triggerLabel(t, k))) });
    case "can":
      return t("channels:bots.app.lines.can", { actions: joinList(line.actions.map((k) => actionLabel(t, k))) });
    case "sendsTo":
      return t("channels:bots.app.lines.sendsTo", { hosts: joinList(line.hosts) });
    case "cannotSendOutside":
      return t("channels:bots.app.marks.noEgress");
    case "keeps":
      return t("channels:bots.app.lines.keeps", { size: formatBytes(line.bytes) });
  }
}

/** One row of the update's diff. */
export function changeText(t: T, c: ManifestChange): string {
  let value = c.value;
  let previous = c.previous ?? "";
  switch (c.field) {
    case "trigger":
      value = triggerLabel(t, c.value);
      break;
    case "channel":
      value = channelLabel(t, c.value);
      break;
    case "history":
      value = formatDays(Number(c.value) || 0);
      previous = formatDays(Number(c.previous) || 0);
      break;
    case "event":
      value = t(`channels:bots.app.events.${c.value as "created" | "edited"}`, { defaultValue: c.value });
      break;
    case "action":
      value = actionLabel(t, c.value);
      break;
    case "state_quota":
      value = formatBytes(Number(c.value) || 0);
      previous = formatBytes(Number(c.previous) || 0);
      break;
    case "command":
      if (c.kind === "increased") {
        const next = c.previous === "request" ? "command" : "request";
        return t("channels:bots.app.changes.command_increased", {
          value: c.value,
          previous: t(`channels:bots.app.commandKinds.${c.previous === "request" ? "request" : "command"}`),
          next: t(`channels:bots.app.commandKinds.${next}`),
        });
      }
      break;
    case "schedule":
      if ((c.kind === "increased" || c.kind === "decreased") && c.every_secs !== undefined) {
        return t(`channels:bots.app.changes.schedule_${c.kind}`, {
          value: c.value,
          previous: c.previous_every_secs !== undefined ? formatPeriod(c.previous_every_secs) : "",
          next: formatPeriod(c.every_secs),
        });
      }
      break;
    default:
      break;
  }
  return t(`channels:bots.app.changes.${c.field}_${c.kind}` as "channels:bots.app.changes.trigger_added", { value, previous, next: value });
}

/** Why the bot is not running, in words; the core's detail for a code this build doesn't know. */
export function reasonText(t: T, code: string, detail: string): string {
  return isAppReasonCode(code) ? t(`channels:bots.app.reasons.${code}`) : detail;
}

/** A failed approval: "look again" for a stale or changed page, else the core's words. */
export function approvalErrorText(t: T, err: unknown): string {
  switch (appApprovalErrorKind(err)) {
    case "stale":
      return t("channels:bots.app.errors.stale");
    case "changed":
      return t("channels:bots.app.errors.changed");
    default:
      return errorMessage(err, t("channels:bots.app.errors.failed"));
  }
}
