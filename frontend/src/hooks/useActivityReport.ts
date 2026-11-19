/**
 * Tell the DS when this desktop is in use (#2056), so it holds the account's phone pushes while
 * the user is here and sends what is still unread once they leave.
 *
 * Reported on change (focus, blur, hide, idle, close) and renewed on input at most once every
 * `RENEW_MS` while active: the DS lets a report lapse after two minutes, so a crashed or
 * disconnected client stops holding pushes on its own. No timer, no heartbeat.
 */
import { useEffect } from "react";
import { reaction } from "mobx";
import { invoke } from "../bridge";
import { startUserActivity, userActivityStore } from "../stores/userActivityStore";
import { activityReportDue } from "../utils/notificationRules";
import { logIgnored } from "../utils/log";

export function useActivityReport(userId: string | null) {
  useEffect(() => startUserActivity(), []);

  useEffect(() => {
    if (!userId) {
      return;
    }
    let last: { active: boolean; sentAt: number } | null = null;
    const send = (active: boolean) => {
      const now = Date.now();
      if (!activityReportDue(last, active, now)) {
        return;
      }
      last = { active, sentAt: now };
      invoke("report_push_activity", { userId, active }).catch(logIgnored);
    };
    const dispose = reaction(
      () => [userActivityStore.isActive, userActivityStore.lastInputAt] as const,
      ([active]) => send(active),
      { fireImmediately: true },
    );
    const onLeave = () => send(false);
    window.addEventListener("pagehide", onLeave);
    return () => {
      dispose();
      window.removeEventListener("pagehide", onLeave);
      // Signed out or switched account: this user is no longer here.
      send(false);
    };
  }, [userId]);
}
