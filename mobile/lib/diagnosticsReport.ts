// The diagnostics report (#1305) split for display and the clipboard: the header, then only the NEWEST
// lines. The whole log can reach ~1 MB; one string that size is cut off by the screen and the
// clipboard, which dropped the newest lines first. "Save as file" carries the whole log. A copy of
// `frontend/src/utils/diagnosticsReport.ts` (mobile imports no frontend code). Import-free for node tests.

/** Most characters of log the preview and the clipboard carry. */
export const RECENT_LOG_CHARS = 48_000;

export interface RecentReport {
  /** Header + the newest whole lines, oldest of them first. */
  text: string;
  /** Lines left out; 0 when the whole log fits. */
  omitted: number;
}

export function recentReport(report: string, maxChars: number = RECENT_LOG_CHARS): RecentReport {
  const split = report.indexOf("----\n");
  if (split < 0) {
    return { text: report, omitted: 0 };
  }
  const header = report.slice(0, split + 5);
  const log = report.slice(split + 5);
  if (log.length <= maxChars) {
    return { text: report, omitted: 0 };
  }
  // Start on a whole line inside the newest `maxChars`.
  const start = log.indexOf("\n", log.length - maxChars - 1) + 1;
  const omitted = log.slice(0, start).split("\n").length - 1;
  return { text: header + log.slice(start), omitted };
}

/** `pollis-diagnostics-20261103-1716.txt`, in UTC like the log's own stamps. */
export function diagnosticsFileName(now: Date = new Date()): string {
  const stamp = now.toISOString().slice(0, 16).replace(/[-:]/g, "").replace("T", "-");
  return `pollis-diagnostics-${stamp}.txt`;
}
