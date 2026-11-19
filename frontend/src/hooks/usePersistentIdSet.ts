import { useCallback, useEffect, useState } from "react";

/**
 * A set of ids kept in `localStorage` under `key`: this device only, never synced. The sidebar's
 * collapsed groups and collapsed channel categories (#2031) are both one of these.
 */
export function usePersistentIdSet(key: string): [ReadonlySet<string>, (id: string) => void] {
  const [ids, setIds] = useState<Set<string>>(() => {
    try {
      const raw = localStorage.getItem(key);
      return new Set(raw ? (JSON.parse(raw) as string[]) : []);
    } catch {
      return new Set();
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(key, JSON.stringify([...ids]));
    } catch {
      /* localStorage unavailable — non-fatal */
    }
  }, [key, ids]);

  const toggle = useCallback((id: string) => {
    setIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  return [ids, toggle];
}
