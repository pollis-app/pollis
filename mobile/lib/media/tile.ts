// One mounted media consumer's resolve lifecycle (#1826), pure: hooks/useMediaUri wires it to the cache.
//
// - Every attempt is one acquire paired with one release (a failed acquire still holds a count).
// - A failure is never sticky: while failed, the tile listens for that content hash, and the
//   first successful decrypt by any other consumer (the viewer, another row) re-runs the attempt,
//   which the cache answers at once. No timer, no polling.
// - A new mount starts from scratch: it peeks, then asks the cache, which retries a failed fetch.

export interface MediaTileState {
  uri: string | null;
  loading: boolean;
  error: Error | null;
}

export interface MediaTileSource {
  /** The file already on disk for `hash`, else null. */
  peek(hash: string): string | null;
  /** Take a reference and resolve; pair every call with one `release`. */
  resolve(): Promise<string>;
  /** Drop the reference taken by the attempt that started in `generation`. */
  release(generation: number): void;
  generation(): number;
  /** Listen for a successful resolve of `hash` by anyone; returns the unsubscribe. */
  onResolved(hash: string, listener: () => void): () => void;
}

/** Start resolving; `emit` receives every state change. Returns the teardown for unmount. */
export function startMediaTile(
  hash: string | null,
  source: MediaTileSource,
  emit: (state: MediaTileState) => void,
): () => void {
  let active = true;
  let attemptGeneration = source.generation();
  let unsubscribe: (() => void) | null = null;

  function stopListening(): void {
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  }

  function retry(): void {
    stopListening();
    source.release(attemptGeneration);
    attempt();
  }

  function attempt(): void {
    // A release after a sign-out clear must not touch the next session's counts.
    attemptGeneration = source.generation();
    const cached = hash ? source.peek(hash) : null;
    emit(
      cached
        ? { uri: cached, loading: false, error: null }
        : { uri: null, loading: true, error: null },
    );
    source.resolve().then(
      (uri) => {
        if (active) {
          emit({ uri, loading: false, error: null });
        }
      },
      (err: unknown) => {
        if (!active) {
          return;
        }
        emit({
          uri: null,
          loading: false,
          error: err instanceof Error ? err : new Error(String(err)),
        });
        if (!hash) {
          return;
        }
        unsubscribe = source.onResolved(hash, retry);
        // Someone else may have landed it between our failure and the subscribe.
        if (source.peek(hash)) {
          retry();
        }
      },
    );
  }

  attempt();

  return () => {
    active = false;
    stopListening();
    // The last outstanding release unlinks the decrypted file.
    source.release(attemptGeneration);
  };
}
