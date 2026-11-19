// Which tags tagged avatars are drawing, read only by `AvatarProbe` for Maestro. Pure.
// A tag is the avatar's testID plus what it shows: `-photo`, `-presence-online`, `-presence-offline`.

const shown = new Map<string, number>();
const listeners = new Set<() => void>();
let version = 0;

function notify(): void {
  version += 1;
  for (const l of listeners) {
    l();
  }
}

/** An avatar is drawing `tag`. Returns the withdrawal, for unmount or a change. */
export function reportAvatarTag(tag: string): () => void {
  shown.set(tag, (shown.get(tag) ?? 0) + 1);
  notify();
  let withdrawn = false;
  return () => {
    if (withdrawn) {
      return;
    }
    withdrawn = true;
    const n = (shown.get(tag) ?? 1) - 1;
    if (n <= 0) {
      shown.delete(tag);
    } else {
      shown.set(tag, n);
    }
    notify();
  };
}

export function avatarTagsShowing(): string[] {
  return [...shown.keys()].sort();
}

export function subscribeAvatarTags(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function avatarTagsVersion(): number {
  return version;
}
