/*
 * Sender-generated link previews (#1987): which link gets one, when a received one may be drawn,
 * and the composer's draft lifecycle. mobile/lib/linkPreview.ts is a byte-for-byte copy (mobile
 * imports no frontend TypeScript); mobile/tests/link-preview.test.ts fails if the two differ.
 *
 * The sender's own device fetches the page (in pollis-core); recipients never contact the site.
 * Pure: no React, no bridge. The bridge calls are injected, so node tests drive it directly.
 */

import { findUrls, parseLink } from "./links.ts";

/** A staged thumbnail of a draft; `dataUrl` is for the composer card only. */
export interface LinkPreviewDraftImage {
  stagedId: string;
  width: number;
  height: number;
  dataUrl: string;
}

/** `fetch_link_preview`'s answer, handed back to `send_message` / `edit_message` as `linkPreview`. */
export interface LinkPreviewDraft {
  url: string;
  title: string;
  description: string | null;
  image: LinkPreviewDraftImage | null;
}

/** The thumbnail reference inside a received preview: the field names of an `_att` entry. */
export interface LinkPreviewImageWire {
  key: string;
  url?: string;
  hash: string;
  kv: number;
  sha: string;
  ct: string;
  size: number;
  w: number;
  h: number;
  bh?: string;
}

/** A message row's `link_preview`. `image` is absent while the sender's upload is pending. */
export interface LinkPreviewWire {
  url: string;
  title: string;
  description?: string | null;
  image?: LinkPreviewImageWire | null;
}

/** Typing pause before a link in the draft is fetched; a paste fetches at once. */
export const PREVIEW_DEBOUNCE_MS = 600;

/** The destination of every link in `text`, in order, as `parseLink` normalizes it. */
export function linkTargets(text: string): { href: string; host: string }[] {
  const out: { href: string; host: string }[] = [];
  for (const { url } of findUrls(text) ?? []) {
    const target = parseLink(url);
    if (target !== null) {
      out.push({ href: target.href, host: target.host });
    }
  }
  return out;
}

/** The one link a draft may preview: the first link `parseLink` accepts that opens over https. */
export function previewCandidate(text: string): string | null {
  for (const { href } of linkTargets(text)) {
    if (href.startsWith("https://")) {
      return href;
    }
  }
  return null;
}

/**
 * A received preview is drawn only for a link the reader can see in the body: its `url` must be
 * exactly the `href` of one of the body's links. The domain line is that body link's host, never
 * anything from the preview. Null means draw nothing.
 */
export function matchPreviewLink(
  text: string | null | undefined,
  preview: { url: string } | null | undefined,
): { href: string; host: string } | null {
  if (!text || !preview || typeof preview.url !== "string") {
    return null;
  }
  for (const target of linkTargets(text)) {
    if (target.href === preview.url) {
      return target;
    }
  }
  return null;
}

/** What the composer shows: nothing, a card loading for `url`, or the finished card. */
export type LinkPreviewState =
  | { status: "none" }
  | { status: "loading"; url: string }
  | { status: "ready"; url: string; draft: LinkPreviewDraft };

export const NO_LINK_PREVIEW: LinkPreviewState = { status: "none" };

export interface LinkPreviewDeps {
  /** `fetch_link_preview`; null (or a rejection) means no preview. */
  fetch: (url: string) => Promise<LinkPreviewDraft | null>;
  /** `discard_staged_attachment`, for a thumbnail that will not be sent. */
  discard: (stagedId: string) => void;
  onChange: (state: LinkPreviewState) => void;
  schedule?: (run: () => void, ms: number) => unknown;
  cancel?: (handle: unknown) => void;
}

/**
 * One draft's preview. `update` on every text change; `remove` for the card's ×; `takeForSend`
 * at send (never waits for a fetch still running); `reset` when the draft is abandoned.
 */
export class LinkPreviewComposer {
  private state: LinkPreviewState = NO_LINK_PREVIEW;
  // Removed with ×: never offered again in this draft.
  private readonly suppressed = new Set<string>();
  // The URL the current state (or a pending timer, or a fetch that found nothing) is about.
  private settledUrl: string | null = null;
  private timer: unknown = null;
  // Bumped whenever an answer in flight stops being wanted.
  private generation = 0;
  private lastText = "";
  private readonly deps: LinkPreviewDeps;
  private readonly schedule: (run: () => void, ms: number) => unknown;
  private readonly cancel: (handle: unknown) => void;

  constructor(deps: LinkPreviewDeps) {
    this.deps = deps;
    this.schedule = deps.schedule ?? ((run, ms) => setTimeout(run, ms));
    this.cancel =
      deps.cancel ??
      ((handle) => {
        clearTimeout(handle as ReturnType<typeof setTimeout>);
      });
  }

  get current(): LinkPreviewState {
    return this.state;
  }

  update(text: string, enabled: boolean): void {
    // More than one character arriving at once is a paste (or a restored draft): no debounce.
    const pasted = text.length - this.lastText.length > 1;
    this.lastText = text;
    const candidate = enabled ? previewCandidate(text) : null;
    if (candidate === null || this.suppressed.has(candidate)) {
      this.clear();
      return;
    }
    if (candidate === this.settledUrl) {
      return;
    }
    this.clear();
    this.settledUrl = candidate;
    if (pasted) {
      this.start(candidate);
      return;
    }
    this.timer = this.schedule(() => {
      this.timer = null;
      this.start(candidate);
    }, PREVIEW_DEBOUNCE_MS);
  }

  /** The card's ×: drop it and never offer this URL again in this draft. */
  remove(): void {
    if (this.settledUrl !== null) {
      this.suppressed.add(this.settledUrl);
    }
    this.clear();
  }

  /** The finished preview for `text`, if its link is still in it; the draft then starts over. */
  takeForSend(text: string): LinkPreviewDraft | null {
    const state = this.state;
    let taken: LinkPreviewDraft | null = null;
    if (state.status === "ready" && linkTargets(text).some((t) => t.href === state.draft.url)) {
      taken = state.draft;
      // The core consumes the staged thumbnail on send; it is not ours to discard any more.
      this.set(NO_LINK_PREVIEW);
    }
    this.reset();
    return taken;
  }

  /** Abandon the draft (sent, cancelled, conversation switched): release whatever is staged. */
  reset(): void {
    this.suppressed.clear();
    this.lastText = "";
    this.clear();
  }

  private start(url: string): void {
    this.generation += 1;
    const generation = this.generation;
    this.set({ status: "loading", url });
    let request: Promise<LinkPreviewDraft | null>;
    try {
      request = this.deps.fetch(url);
    } catch {
      request = Promise.resolve(null);
    }
    request.then(
      (draft) => this.settle(generation, url, draft ?? null),
      () => this.settle(generation, url, null),
    );
  }

  private settle(generation: number, url: string, draft: LinkPreviewDraft | null): void {
    if (generation !== this.generation) {
      // Not wanted any more: release its thumbnail now.
      this.release(draft);
      return;
    }
    if (draft === null || typeof draft.title !== "string" || draft.title === "") {
      this.release(draft);
      // Stay settled on `url`, so the same link is not fetched again.
      this.set(NO_LINK_PREVIEW);
      return;
    }
    // Pinned to the link as the body has it, which is what a reader's device matches.
    this.set({ status: "ready", url, draft: { ...draft, url } });
  }

  private clear(): void {
    if (this.timer !== null) {
      this.cancel(this.timer);
      this.timer = null;
    }
    this.generation += 1;
    this.settledUrl = null;
    if (this.state.status === "ready") {
      this.release(this.state.draft);
    }
    if (this.state.status !== "none") {
      this.set(NO_LINK_PREVIEW);
    }
  }

  private release(draft: LinkPreviewDraft | null): void {
    if (draft?.image) {
      this.deps.discard(draft.image.stagedId);
    }
  }

  private set(state: LinkPreviewState): void {
    this.state = state;
    this.deps.onChange(state);
  }
}
