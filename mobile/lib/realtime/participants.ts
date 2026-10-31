// Who published a data packet (#1635). LiveKit identities are per-room DS pseudonyms (#836), so the
// sender is resolved through the core, once per room and identity, like desktop's realtime loop.

export type PacketSender = { userId: string; username: string | null };

type Resolve = (
  room: string,
  identities: string[],
) => Promise<Array<{ identity: string; userId: string; username?: string | null }>>;

/** A per-room cache over `resolve`. `null` = the DS could not attribute it (a fabricated identity). */
export function createParticipantResolver(resolve: Resolve) {
  const cache = new Map<string, PacketSender | null>();
  const inflight = new Map<string, Promise<PacketSender | null>>();

  return function resolveSender(room: string, identity: string): Promise<PacketSender | null> {
    const key = `${room}\u0000${identity}`;
    if (cache.has(key)) {
      return Promise.resolve(cache.get(key) ?? null);
    }
    const pending = inflight.get(key);
    if (pending) {
      return pending;
    }
    const lookup = resolve(room, [identity])
      .then((rows) => {
        const row = rows.find((r) => r.identity === identity);
        const sender = row ? { userId: row.userId, username: row.username ?? null } : null;
        cache.set(key, sender);
        return sender;
      })
      // Offline, or a store binary without the command: drop this packet, ask again on the next.
      .catch(() => null)
      .finally(() => {
        inflight.delete(key);
      });
    inflight.set(key, lookup);
    return lookup;
  };
}
