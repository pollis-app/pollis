// Renderer-side livekit-client connection that drives screen-share under
// Electron. Phase 6 of the migration: Chromium gives us WebRTC in the
// webview, so we can do screen-share entirely in JS — receivers render
// via `<video srcObject>` at hardware-decoded 60fps instead of the Tauri
// path that pushed I420 frames over IPC at <1fps.
//
// Architecture:
//   - When the Rust voice client joins a room, this JS client opens a
//     second connection to the SAME room as `{userId}:view`. The JWT is
//     marked `hidden: true` so other peers don't see this as a duplicate
//     of the voice participant in their rosters.
//   - It subscribes to remote video tracks (only the ones flagged
//     `Track.Source.ScreenShare`) and stashes each one on a Map keyed by
//     publisher identity. React tiles read this via useSyncExternalStore.
//   - It also publishes the local screen-share when the user starts a
//     share — `getDisplayMedia` in the renderer → `publishTrack` on the
//     same Room. The local track is exposed under the reserved
//     `LOCAL_PREVIEW_KEY` so the existing preview tile works unchanged.
//   - The Zustand `screenShareRemotes` map is kept in sync by translating
//     publisher identity `{userId}:view` → `voice-{userId}`, so tiles that
//     look up by voice identity (the existing path) keep working.
//
// Lifecycle reconciler mirrors `VoiceSessionManager`: declarative intent
// (`activeVoiceChannelId` + `voicePhase === 'joined'`), reconcile loop
// coalesces rapid changes so a fast join/leave/join doesn't leak a stale
// connection.

import {
  LocalTrackPublication,
  Room,
  RoomEvent,
  Track,
} from 'livekit-client';
import type {
  LocalVideoTrack,
  RemoteParticipant,
  RemoteTrack,
  RemoteTrackPublication,
} from 'livekit-client';

import { invoke } from '../bridge';
import { hasElectron } from '../bridge/runtime';
import { useAppStore } from '../stores/appStore';
import { LOCAL_PREVIEW_KEY } from './screenShareSession';

// ── Public types ─────────────────────────────────────────────────────────────

/**
 * Snapshot of every active screen-share track this client has visibility
 * of (remote subscribed + locally published). Keys are tile identifiers:
 *   - For remote tracks: the voice identity `voice-{userId}` (derived
 *     from the publisher identity `{userId}:view`), so the keys line up
 *     with `screenShareRemotes` in the Zustand store.
 *   - For the local publish: the sentinel `LOCAL_PREVIEW_KEY` so the
 *     existing in-tile preview renders unchanged.
 */
export type TrackMap = ReadonlyMap<string, MediaStreamTrack>;

type Listener = () => void;

interface ViewIntent {
  channelId: string;
  userId: string;
  displayName: string;
}

// ── Identity helpers ─────────────────────────────────────────────────────────

/** Derive the voice identity that other parts of the app key on
 *  (`voice-{userId}`) from a view participant identity (`{userId}:view`).
 *  Returns null for any other shape so we don't accidentally surface
 *  tracks from rooms / clients that aren't using the view scheme. */
function voiceIdentityFromView(identity: string): string | null {
  const idx = identity.lastIndexOf(':view');
  if (idx === -1 || idx + ':view'.length !== identity.length) {
    return null;
  }
  const userId = identity.slice(0, idx);
  if (!userId) {
    return null;
  }
  return `voice-${userId}`;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function sameIntent(a: ViewIntent | null, b: ViewIntent | null): boolean {
  if (a === b) {
    return true;
  }
  if (!a || !b) {
    return false;
  }
  return (
    a.channelId === b.channelId &&
    a.userId === b.userId &&
    a.displayName === b.displayName
  );
}

// ── The manager ──────────────────────────────────────────────────────────────

class LiveKitView {
  private intent: ViewIntent | null = null;
  private current: ViewIntent | null = null;
  private currentRoom: Room | null = null;
  private reconciling = false;

  private tracks = new Map<string, MediaStreamTrack>();
  /** Width × height per active remote track. Pushed into the Zustand
   *  `screenShareRemotes` mirror so existing layout that sizes the
   *  preview tile from initial dimensions keeps working. */
  private trackDims = new Map<string, { width: number; height: number }>();
  private listeners = new Set<Listener>();
  /** Stable snapshot for useSyncExternalStore — only re-allocated when
   *  the underlying map actually changes. */
  private snapshot: TrackMap = new Map();

  // ── Local publish state ───────────────────────────────────────────────────
  /** The track we're currently publishing, if any. Held so unpublish()
   *  can stop and clean it up. */
  private localTrack: LocalVideoTrack | null = null;
  private localPublication: LocalTrackPublication | null = null;

  // ── Subscription API ──────────────────────────────────────────────────────

  /** useSyncExternalStore subscribe. */
  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** useSyncExternalStore snapshot. Stable identity until the track set
   *  actually changes, so React only re-renders consumers of identities
   *  whose tracks moved. */
  getSnapshot(): TrackMap {
    return this.snapshot;
  }

  /** Direct lookup for a single key. Cheaper than allocating a new
   *  selector callback per consumer when a tile only cares about one. */
  getTrack(key: string): MediaStreamTrack | undefined {
    return this.tracks.get(key);
  }

  // ── Intent management ─────────────────────────────────────────────────────

  /**
   * Set the channel this view client should be connected to. Pass `null`
   * to disconnect. Idempotent and safe to call rapidly — concurrent
   * changes coalesce and only the latest intent is honored.
   */
  setIntent(target: ViewIntent | null): void {
    if (sameIntent(target, this.intent)) {
      return;
    }
    this.intent = target;
    void this.reconcile();
  }

  // ── Publish API ───────────────────────────────────────────────────────────

  /**
   * Publish a screen-share track on the current Room. Throws if the view
   * connection isn't joined yet (i.e. voice isn't in `joined` phase). The
   * caller is responsible for sourcing the track via `getDisplayMedia`.
   */
  async publishScreenShare(track: MediaStreamTrack): Promise<void> {
    const room = this.currentRoom;
    if (!room) {
      throw new Error('publishScreenShare: not connected to a room');
    }
    if (this.localPublication) {
      // Replace the existing publish — stop the old track first so the
      // browser releases the capture handle.
      await this.unpublishScreenShare();
    }
    const publication = await room.localParticipant.publishTrack(track, {
      source: Track.Source.ScreenShare,
      // Force VP8 to avoid a Chromium SDP collision on payload_type 35
      // when AV1 and H.264 are both offered with the same dynamic PT
      // ("A BUNDLE group contains a codec collision for
      // payload_type='35'"). VP8 is the universally supported screen-
      // share codec and has no PT conflict. Switch to vp9/av1 later
      // once Chromium's PT allocator settles.
      videoCodec: 'vp8',
      // Disable simulcast for screen-share — high-bitrate single layer
      // matches text legibility better than scaled-down spatial layers.
      simulcast: false,
    });
    this.localPublication = publication;
    // The publication wraps the MediaStreamTrack as a LocalVideoTrack.
    const localTrack = publication.track as LocalVideoTrack | undefined;
    if (localTrack) {
      this.localTrack = localTrack;
    }
    // Surface the local track under LOCAL_PREVIEW_KEY so the in-tile
    // preview renders. The browser will stop the track if the user clicks
    // "Stop sharing" in the system overlay — listen for that and clean up.
    track.addEventListener('ended', () => {
      void this.unpublishScreenShare();
      // Notify the store so VoiceMemberTile flips back to the avatar.
      const store = useAppStore.getState();
      if (store.screenShareLocalActive) {
        store.setScreenShareLocalActive(false);
        store.setScreenShareLocalDimensions(null);
        store.setScreenShareMode('idle');
      }
    });
    this.tracks.set(LOCAL_PREVIEW_KEY, track);
    const settings = track.getSettings();
    if (
      typeof settings.width === 'number' &&
      typeof settings.height === 'number'
    ) {
      this.trackDims.set(LOCAL_PREVIEW_KEY, {
        width: settings.width,
        height: settings.height,
      });
    }
    this.emit();
  }

  /** Stop publishing the local screen-share, if any. Safe to call when
   *  nothing is published. */
  async unpublishScreenShare(): Promise<void> {
    const room = this.currentRoom;
    const publication = this.localPublication;
    const localTrack = this.localTrack;
    this.localPublication = null;
    this.localTrack = null;
    if (room && localTrack) {
      try {
        await room.localParticipant.unpublishTrack(localTrack, true);
      } catch (e) {
        console.warn('[livekit-view] unpublishTrack:', e);
      }
    } else if (room && publication?.track) {
      try {
        await room.localParticipant.unpublishTrack(publication.track, true);
      } catch (e) {
        console.warn('[livekit-view] unpublishTrack:', e);
      }
    }
    if (this.tracks.delete(LOCAL_PREVIEW_KEY)) {
      this.trackDims.delete(LOCAL_PREVIEW_KEY);
      this.emit();
    }
  }

  // ── Reconciliation ────────────────────────────────────────────────────────

  private async reconcile(): Promise<void> {
    if (this.reconciling) {
      return;
    }
    this.reconciling = true;
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const target = this.intent;
        const current = this.current;

        if (sameIntent(target, current)) {
          return;
        }

        if (current) {
          await this.executeLeave();
          continue;
        }

        if (target === null) {
          return;
        }

        const ok = await this.executeJoin(target);
        if (!ok) {
          if (sameIntent(this.intent, target)) {
            this.intent = null;
            return;
          }
          continue;
        }
      }
    } finally {
      this.reconciling = false;
    }
  }

  private async executeJoin(target: ViewIntent): Promise<boolean> {
    const identity = `${target.userId}:view`;
    let token: string;
    let url: string;
    try {
      [token, url] = await Promise.all([
        invoke<string>('get_livekit_view_token', {
          roomName: target.channelId,
          identity,
          displayName: target.displayName,
        }),
        invoke<string>('get_livekit_url'),
      ]);
    } catch (e) {
      console.error('[livekit-view] token/url fetch failed:', e);
      return false;
    }

    if (!url) {
      // LiveKit not configured — there's no view stream to connect to.
      // Treat as success so we don't busy-loop reconciling.
      this.current = target;
      return true;
    }

    const room = new Room({
      adaptiveStream: true,
      dynacast: true,
    });

    this.wireRoomEvents(room);

    try {
      // autoSubscribe:false so the view client never subscribes to
      // *audio* tracks (those belong to the Rust voice client) — they
      // would add audio codec entries to this PC's SDP, and Chromium 130
      // assigns those a PT that collides with screen-share video's PT in
      // the BUNDLE group ("A BUNDLE group contains a codec collision for
      // payload_type='35'"), which torpedoes screen-share publish. Manual
      // per-track subscription below opts in only to screen-share video.
      await room.connect(url, token, { autoSubscribe: false });
    } catch (e) {
      console.error('[livekit-view] connect failed:', e);
      try {
        await room.disconnect();
      } catch {
        // ignore
      }
      return false;
    }

    if (!sameIntent(this.intent, target)) {
      try {
        await room.disconnect();
      } catch {
        // ignore
      }
      return true;
    }

    this.currentRoom = room;
    this.current = target;

    // Opt in to any existing screen-share publications (subscribe-only-
    // what-we-need pattern). Audio publications are ignored — see the
    // autoSubscribe:false rationale above.
    for (const participant of room.remoteParticipants.values()) {
      for (const publication of participant.trackPublications.values()) {
        if (
          publication.kind === Track.Kind.Video &&
          publication.source === Track.Source.ScreenShare
        ) {
          publication.setSubscribed(true);
        }
      }
    }

    return true;
  }

  private async executeLeave(): Promise<void> {
    const room = this.currentRoom;
    this.currentRoom = null;
    this.current = null;
    // Unpublish before disconnect so the SDK has a chance to stop the
    // capture cleanly (frees the OS capture handle, removes the system
    // "you're sharing" indicator immediately).
    if (this.localPublication || this.localTrack) {
      try {
        await this.unpublishScreenShare();
      } catch (e) {
        console.warn('[livekit-view] unpublish on leave:', e);
      }
    }
    if (room) {
      try {
        await room.disconnect();
      } catch (e) {
        console.warn('[livekit-view] disconnect failed:', e);
      }
    }
    // Clear remote tracks; preserve any LOCAL_PREVIEW_KEY only if a new
    // share is in flight (none here — `current` cleared above).
    const hadAny = this.tracks.size > 0;
    this.tracks.clear();
    this.trackDims.clear();
    if (hadAny) {
      this.emit();
    }
  }

  // ── Room event wiring ─────────────────────────────────────────────────────

  private wireRoomEvents(room: Room): void {
    // With autoSubscribe:false, new publications arrive as TrackPublished.
    // Filter to video screen-share and opt in; everything else (audio,
    // camera if it ever appears) is ignored — keeps audio codecs out of
    // this PC's SDP, which prevents the PT=35 BUNDLE collision.
    room.on(RoomEvent.TrackPublished, (publication) => {
      if (
        publication.kind === Track.Kind.Video &&
        publication.source === Track.Source.ScreenShare
      ) {
        publication.setSubscribed(true);
      }
    });
    room.on(RoomEvent.TrackSubscribed, (track, publication, participant) => {
      this.handleTrackSubscribed(track, publication, participant);
    });
    room.on(RoomEvent.TrackUnsubscribed, (track, _publication, participant) => {
      this.handleTrackUnsubscribed(track, participant);
    });
    room.on(RoomEvent.ParticipantDisconnected, (participant) => {
      const key = voiceIdentityFromView(participant.identity);
      if (key && this.tracks.delete(key)) {
        this.trackDims.delete(key);
        this.emit();
      }
    });
    room.on(RoomEvent.Disconnected, () => {
      const hadAny = this.tracks.size > 0;
      this.tracks.clear();
      this.trackDims.clear();
      this.current = null;
      this.currentRoom = null;
      if (hadAny) {
        this.emit();
      }
      void this.reconcile();
    });
  }

  private handleTrackSubscribed(
    track: RemoteTrack,
    publication: RemoteTrackPublication,
    participant: RemoteParticipant,
  ): void {
    if (publication.kind !== Track.Kind.Video) {
      return;
    }
    if (publication.source !== Track.Source.ScreenShare) {
      return;
    }
    const mediaTrack = track.mediaStreamTrack;
    if (!mediaTrack) {
      return;
    }
    // Map publisher identity (`{userId}:view`) to the voice identity
    // (`voice-{userId}`) so the keys line up with the existing
    // `screenShareRemotes` plumbing.
    const key = voiceIdentityFromView(participant.identity);
    if (!key) {
      // Not a view-scheme publisher — likely the Rust client itself or
      // some other client; ignore so we don't double-render.
      return;
    }
    this.tracks.set(key, mediaTrack);
    // Capture initial dimensions for the layout. Some browsers report 0
    // until the first frame lands; that's OK — the <video> resizes to
    // its intrinsic dimensions once frames start flowing.
    const settings = mediaTrack.getSettings();
    if (
      typeof settings.width === 'number' &&
      typeof settings.height === 'number' &&
      settings.width > 0 &&
      settings.height > 0
    ) {
      this.trackDims.set(key, {
        width: settings.width,
        height: settings.height,
      });
    }
    this.emit();
  }

  private handleTrackUnsubscribed(
    _track: RemoteTrack,
    participant: RemoteParticipant,
  ): void {
    const key = voiceIdentityFromView(participant.identity);
    if (!key) {
      return;
    }
    if (this.tracks.delete(key)) {
      this.trackDims.delete(key);
      this.emit();
    }
  }

  // ── Notify ────────────────────────────────────────────────────────────────

  /** Allocate a fresh snapshot from the live map so React's
   *  useSyncExternalStore sees a new reference. Cheap — the map is
   *  small (one entry per active remote share + one local).
   *
   *  Also mirrors the remote portion into Zustand's `screenShareRemotes`
   *  so the existing tile plumbing (`screenShareRemotes[p.identity]`)
   *  picks up new shares without touching every reader. */
  private emit(): void {
    this.snapshot = new Map(this.tracks);
    // Mirror remote keys into the store. Local preview is driven by the
    // existing `screenShareLocalActive` field — don't duplicate it here.
    const store = useAppStore.getState();
    const desired: Record<
      string,
      { trackKey: string; width: number; height: number }
    > = {};
    for (const [key] of this.tracks) {
      if (key === LOCAL_PREVIEW_KEY) {
        continue;
      }
      const dims = this.trackDims.get(key) ?? { width: 0, height: 0 };
      desired[key] = {
        trackKey: key,
        width: dims.width,
        height: dims.height,
      };
    }
    // Replace wholesale only if it differs — avoids needless re-renders
    // for consumers that read the map.
    const current = store.screenShareRemotes;
    const currentKeys = Object.keys(current);
    const desiredKeys = Object.keys(desired);
    let changed = currentKeys.length !== desiredKeys.length;
    if (!changed) {
      for (const k of desiredKeys) {
        const a = current[k];
        const b = desired[k];
        if (!a || a.trackKey !== b.trackKey || a.width !== b.width || a.height !== b.height) {
          changed = true;
          break;
        }
      }
    }
    if (changed) {
      useAppStore.setState({ screenShareRemotes: desired });
    }
    for (const listener of this.listeners) {
      try {
        listener();
      } catch (e) {
        console.error('[livekit-view] listener', e);
      }
    }
  }
}

// ── Singleton ────────────────────────────────────────────────────────────────

export const livekitView = new LiveKitView();

// ── Store wiring ─────────────────────────────────────────────────────────────
//
// Mirror the voice session lifecycle into the view client. Under Tauri
// (WebKitGTK, no WebRTC) the view client stays dormant — the Rust-side
// MJPEG path keeps working. Under Electron we connect/disconnect in
// lockstep with the voice session.

if (typeof window !== 'undefined') {
  const computeIntent = (): ViewIntent | null => {
    if (!hasElectron()) {
      return null;
    }
    const s = useAppStore.getState();
    if (s.voicePhase !== 'joined') {
      return null;
    }
    if (!s.activeVoiceChannelId) {
      return null;
    }
    if (!s.currentUser) {
      return null;
    }
    return {
      channelId: s.activeVoiceChannelId,
      userId: s.currentUser.id,
      displayName: s.currentUser.username ?? s.currentUser.id,
    };
  };

  // Apply once at module load in case voice was already joined when this
  // file is imported (it isn't, today, but the call is cheap and
  // future-proofs against import-order churn).
  livekitView.setIntent(computeIntent());

  useAppStore.subscribe((state, prev) => {
    if (
      state.voicePhase !== prev.voicePhase ||
      state.activeVoiceChannelId !== prev.activeVoiceChannelId ||
      state.currentUser !== prev.currentUser
    ) {
      livekitView.setIntent(computeIntent());
    }
  });
}
