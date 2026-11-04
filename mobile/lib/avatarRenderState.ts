// Which tagged avatars are drawing a photo, read only by `AvatarProbe` for Maestro. Pure.

const shown = new Map<string, number>();
const listeners = new Set<() => void>();
let version = 0;

function notify(): void {
  version += 1;
  for (const l of listeners) {
    l();
  }
}

/** An avatar with `testID` is drawing a photo. Returns the withdrawal, for unmount or a change. */
export function reportAvatarPhoto(testID: string): () => void {
  shown.set(testID, (shown.get(testID) ?? 0) + 1);
  notify();
  let withdrawn = false;
  return () => {
    if (withdrawn) {
      return;
    }
    withdrawn = true;
    const n = (shown.get(testID) ?? 1) - 1;
    if (n <= 0) {
      shown.delete(testID);
    } else {
      shown.set(testID, n);
    }
    notify();
  };
}

export function avatarPhotosShowing(): string[] {
  return [...shown.keys()].sort();
}

export function subscribeAvatarPhotos(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function avatarPhotosVersion(): number {
  return version;
}
