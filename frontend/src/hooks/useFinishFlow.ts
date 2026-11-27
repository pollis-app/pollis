/* Leaving a finished create/edit page (#2129): the form never stays behind Back or ahead of Forward. */

import { useCallback, useMemo } from "react";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { historyNavStore } from "../stores/historyNavStore";

export interface FinishFlow {
  /**
   * Move on to the result. Steps back when the page behind the form is the result itself,
   * else replaces the form's entry with it. Typed exactly like `navigate`.
   */
  finish: ReturnType<typeof useNavigate>;
  /** Return to wherever the form was opened from, dropping it; home when nothing is behind. */
  leave: () => void;
}

/** Back from a form that is still open is untouched; this is only for leaving one. */
export function useFinishFlow(): FinishFlow {
  const router = useRouter();
  const navigate = useNavigate();

  const finish = useCallback<FinishFlow["finish"]>(
    async (opts) => {
      const { pathname } = router.buildLocation(opts as Parameters<typeof router.buildLocation>[0]);
      if (historyNavStore.finishMove(pathname) === "pop") {
        historyNavStore.pop();
        return;
      }
      return navigate({ ...opts, replace: true } as Parameters<typeof navigate>[0]);
    },
    [router, navigate],
  );

  const leave = useCallback(() => {
    if (historyNavStore.canGoBack) {
      historyNavStore.pop();
      return;
    }
    void navigate({ to: "/", replace: true });
  }, [navigate]);

  return useMemo(() => ({ finish, leave }), [finish, leave]);
}
