// When a custom emoji that failed to show tries again (#1562): a short,
// finite schedule, never a poll, then each return to the foreground. Pure, so
// tests/emoji-retry.test.ts pins it.

/** Delay before the retry that follows the Nth consecutive failure (0-based). */
export const EMOJI_RETRY_DELAYS_MS: readonly number[] = [2_000, 10_000, 30_000];

/** The delay before the next attempt, or null once the schedule is spent. */
export function emojiRetryDelay(failures: number): number | null {
  if (failures < 1) {
    return null;
  }
  return EMOJI_RETRY_DELAYS_MS[failures - 1] ?? null;
}

/**
 * Whether returning to the foreground should retry: only an emoji that is
 * showing its fallback and has no timed retry still pending.
 */
export function retryOnForeground(failed: boolean, retryPending: boolean): boolean {
  return failed && !retryPending;
}
