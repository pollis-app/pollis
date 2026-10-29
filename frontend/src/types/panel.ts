// Right-panel URL params (#824, #825, #904); open state is device-local (`stores/rightPanelStore`).

/** The shape the panel is currently rendering. Derived, never parsed. */
export type PanelKind = "members" | "thread" | "pins" | "safety" | "none";

export interface PanelSearch {
  /** ULID of the open thread's root; route-scoped. */
  thread?: string;
  /** `"1"` shows the pinned messages (#99); route-scoped, a thread wins. */
  pins?: "1";
  /** Peer whose safety number is showing (#1563); a thread or pins wins. */
  safety?: string;
  /** Message to scroll to and flash (#850), consumed once. Must be declared here or the router drops it. */
  message?: string;
  /** Query handed from Cmd+K to `/search` (#850). */
  q?: string;
}

/** Parse panel search params; unknown values are dropped, never thrown. */
export function validatePanelSearch(
  search: Record<string, unknown>,
): PanelSearch {
  const out: PanelSearch = {};
  const thread = search.thread;
  if (typeof thread === "string" && thread.length > 0) {
    out.thread = thread;
  }
  if (search.pins === "1") {
    out.pins = "1";
  }
  const safety = search.safety;
  if (typeof safety === "string" && safety.length > 0) {
    out.safety = safety;
  }
  const message = search.message;
  if (typeof message === "string" && message.length > 0) {
    out.message = message;
  }
  const q = search.q;
  if (typeof q === "string" && q.length > 0) {
    out.q = q;
  }
  return out;
}
