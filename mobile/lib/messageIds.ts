// Message ids named ahead of the tap (#2141). Pure. Byte-identical in mobile/lib/messageIds.ts.
//
// A send names its message first (#1335), so its stub and the stored row are one message. That
// id used to be asked of the core on the tap, a bridge round trip before anything could show.
// Now a few are fetched beforehand and the tap takes one: the stub carries the core's id from
// its first frame, so a reply, an edit or a delete can name it before the send settles. The
// core still mints every id (a cryptographic random ULID) and still checks the one it gets.

/** Ids held ready. One tap uses one; a burst of sends past this asks the core directly. */
const MESSAGE_ID_POOL = 3;

export interface MessageIdPool {
  /** A ready id, never handed out twice, or null when none is ready (ask the core then). */
  take(): string | null;
  /** Top the pool up in the background; a failure leaves it as it is. */
  fill(): void;
}

export function createMessageIdPool(
  mint: () => Promise<string>,
  size: number = MESSAGE_ID_POOL,
): MessageIdPool {
  const ready: string[] = [];
  let asking = 0;
  const fill = () => {
    while (ready.length + asking < size) {
      asking += 1;
      mint()
        .then((id) => {
          if (typeof id === "string" && id.length > 0) {
            ready.push(id);
          }
        })
        .catch(() => {
          // Left short: the next take asks the core directly, then fills again.
        })
        .finally(() => {
          asking -= 1;
        });
    }
  };
  return {
    take() {
      const id = ready.shift() ?? null;
      fill();
      return id;
    },
    fill,
  };
}
