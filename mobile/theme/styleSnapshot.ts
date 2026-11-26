// Plain snapshots of the `type.*` steps (#2107). React Native diffs a style prop by identity
// before it looks inside, so a step that stayed one object whose colour was a getter kept the
// first colour it painted: after a background change the Self profile card's name stayed the
// old theme's text colour until a cold start. Each step is the same plain object while the
// generation holds (memoised rows don't re-render for nothing) and a new one after it moves.
export function snapshotSteps<T extends Record<string, object>>(live: T, generation: () => number): T {
  const out = {} as T;
  for (const key of Object.keys(live) as (keyof T)[]) {
    let seen = -1;
    let snapshot: T[keyof T] | null = null;
    Object.defineProperty(out, key, {
      enumerable: true,
      get() {
        const now = generation();
        if (snapshot === null || seen !== now) {
          snapshot = { ...live[key] };
          seen = now;
        }
        return snapshot;
      },
    });
  }
  return out;
}
