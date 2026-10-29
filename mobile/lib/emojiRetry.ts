// A failed custom emoji retries on a short, finite schedule (#1562), then on foreground. Pure.

export const EMOJI_RETRY_DELAYS_MS: readonly number[] = [2_000, 10_000, 30_000];

export function emojiRetryDelay(failures: number): number | null {
  if (failures < 1) {
    return null;
  }
  return EMOJI_RETRY_DELAYS_MS[failures - 1] ?? null;
}

/** Only an emoji showing its fallback with no timed retry pending. */
export function retryOnForeground(failed: boolean, retryPending: boolean): boolean {
  return failed && !retryPending;
}
