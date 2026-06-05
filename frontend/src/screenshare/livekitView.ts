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
  ExternalE2EEKeyProvider,
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
import { autorun } from 'mobx';
import { appStore } from '../stores/appStore';
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
  /** The other user_id in a 1:1 call (`call-*` room). Null for group voice
   *  channels and DMs. Needed by the E2EE key derivation in
   *  `get_voice_e2ee_key` so call-room MLS lookups resolve to the right
   *  DM group. Mirrors VoiceSessionManager's counterpartyUserId. */
  counterpartyUserId: string | null;
}

interface E2eeKeyInfo {
  key: number[];
  key_index: number;
  epoch: number;
  mls_group_id: string;
}

// ── Identity helpers ─────────────────────────────────────────────────────────

/** Derive the voice identity that other parts of the app key on from a
 *  view participant identity. Accepts both shapes:
 *    - `{userId}:view` — legacy (single device per user) → `voice-{userId}`
 *    - `{userId}:{deviceId}:view` — per-device (#140) →
 *      `voice-{userId}:{deviceId}`
 *  The per-device shape carries the publishing device's id through so
 *  the voice tile can match its specific share against
 *  `screenShareRemotes[participant.identity]` directly — when the same
 *  user has multiple devices in a room, each device's voice tile must
 *  show only its own share, not a duplicate of a sibling device's
 *  share. Returns null for any shape that doesn't end in `:view`. */
function voiceIdentityFromView(identity: string): string | null {
  if (!identity.endsWith(':view')) {
    return null;
  }
  const head = identity.slice(0, identity.length - ':view'.length);
  if (!head) {
    return null;
  }
  // `head` is either `{userId}` (legacy) or `{userId}:{deviceId}` (per-
  // device). Mirror the voice-identity scheme by carrying the whole
  // head through with the `voice-` prefix.
  return `voice-${head}`;
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
  /** Active E2EE key provider for the current room, so the epoch-rotation
   *  hook (driven by the Rust `voice_e2ee_key_rotated` event) can call
   *  `setKey` on it when MLS commits advance the group epoch. Null when
   *  no room is connected or when the room was joined unencrypted. */
  private currentKeyProvider: ExternalE2EEKeyProvider | null = null;
  /** MLS group whose exporter secret backs `currentKeyProvider`. Used to
   *  ignore rotation events for unrelated groups (the Rust side filters
   *  too, but this is cheap and keeps the renderer honest). */
  private currentE2eeMlsGroupId: string | null = null;
  /** This device's stable `device_id`, used to build the per-device view
   *  identity `{userId}:{deviceId}:view` so two devices of the same user
   *  don't collide on LiveKit's per-room identity uniqueness check —
   *  same reason `VoiceSessionManager` carries its own deviceId field
   *  (#140). Lazily fetched once; stays null only if the backend can't
   *  supply one, in which case we fall back to the legacy `{userId}:view`
   *  identity (single-device user). */
  private deviceId: string | null = null;

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

  // ── Per-track stats (FPS + decoded dimensions) ────────────────────────────
  //
  // RemoteVideoTile feeds these via requestVideoFrameCallback on the
  // <video> element. The Tauri-era stats path (screenShareSession's
  // FrameListener) doesn't fire under Electron because no frames flow
  // through the Rust channel — livekit-client + Chromium own the decode
  // pipeline now. Mirroring stats here gives useScreenShareStats a single
  // source of truth per runtime.
  private statsByKey = new Map<
    string,
    { fps: number; width: number; height: number }
  >();
  private statsListeners = new Map<
    string,
    Set<(s: { fps: number; width: number; height: number }) => void>
  >();

  recordStats(
    key: string,
    stats: { fps: number; width: number; height: number },
  ): void {
    this.statsByKey.set(key, stats);
    const set = this.statsListeners.get(key);
    if (set) {
      for (const fn of set) {
        fn(stats);
      }
    }
  }

  clearStats(key: string): void {
    this.statsByKey.delete(key);
    const set = this.statsListeners.get(key);
    if (set) {
      const zero = { fps: 0, width: 0, height: 0 };
      for (const fn of set) {
        fn(zero);
      }
    }
  }

  onStats(
    key: string,
    cb: (s: { fps: number; width: number; height: number }) => void,
  ): () => void {
    let set = this.statsListeners.get(key);
    if (!set) {
      set = new Set();
      this.statsListeners.set(key, set);
    }
    set.add(cb);
    const current = this.statsByKey.get(key);
    if (current) {
      cb(current);
    }
    return () => {
      const s = this.statsListeners.get(key);
      if (s) {
        s.delete(cb);
        if (s.size === 0) {
          this.statsListeners.delete(key);
        }
      }
    };
  }

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
    console.info('[livekit-view] publishScreenShare: calling publishTrack', {
      trackId: track.id,
      trackKind: track.kind,
      trackLabel: track.label,
      settings: track.getSettings(),
    });
    const t0 = performance.now();
    const publication = await room.localParticipant.publishTrack(track, {
      source: Track.Source.ScreenShare,
      // Disable simulcast for screen-share — high-bitrate single layer
      // matches text legibility better than scaled-down spatial layers.
      simulcast: false,
      // CRITICAL: screen-share reads `screenShareEncoding`, NOT
      // `videoEncoding` — livekit-client's computeVideoEncodings() swaps to
      // the screenShareEncoding field whenever the source is ScreenShare.
      // Setting videoEncoding here is silently ignored, leaving the default
      // `h1080fps15` preset (1080p @ 15fps) — that was the cross-platform
      // 15fps cap. These are ceilings, not targets: TWCC ramps toward them
      // when the link sustains it and backs off on loss. 8 Mbps is
      // comfortable for 1080p60. maintain-framerate biases toward smooth
      // motion over crisp text under pressure.
      screenShareEncoding: {
        maxFramerate: 60,
        maxBitrate: 8_000_000,
        priority: 'high',
      },
      degradationPreference: 'maintain-framerate',
    });
    console.info('[livekit-view] publishScreenShare: publishTrack resolved', {
      elapsedMs: Math.round(performance.now() - t0),
      sid: publication.trackSid,
      source: publication.source,
    });
    this.localPublication = publication;
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
      appStore.shareStopped();
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
    console.info('[livekit-view] publishScreenShare: emitting + done', {
      localPreviewKey: LOCAL_PREVIEW_KEY,
      tracksSize: this.tracks.size,
    });
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
    // Lazy-fetch device_id once per process (it's stable after login).
    // Without this, two devices of the same user join the LiveKit view
    // room as `{userId}:view` and kick each other out in a reconnect
    // storm — the audit case the user hit cross-machine on PR #371.
    if (!this.deviceId) {
      try {
        this.deviceId = (await invoke<string | null>('get_device_id')) ?? null;
      } catch (e) {
        console.warn('[livekit-view] get_device_id failed (using legacy identity):', e);
      }
    }
    const identity = this.deviceId
      ? `${target.userId}:${this.deviceId}:view`
      : `${target.userId}:view`;
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

    // E2EE setup. Derive the shared MLS key the Rust voice path already
    // uses (`pollis/voice/v1` exporter from the channel's MLS group), feed
    // it into livekit-client's ExternalE2EEKeyProvider. Both publisher
    // and subscriber `:view` clients are MLS members of the same group,
    // so they derive identical keys and decrypt each other's screen-share
    // video. Audio frames still ride the Rust voice path's own E2EE.
    //
    // If key fetch fails (MLS not loaded yet, call-room without
    // counterparty, etc.), fall back to unencrypted — the screen-share
    // still works, just not E2EE. Better to surface video at all than
    // hard-fail. Log loud so it's visible in production telemetry.
    let keyProvider: ExternalE2EEKeyProvider | null = null;
    let keyInfo: E2eeKeyInfo | null = null;
    try {
      keyInfo = await invoke<E2eeKeyInfo>('get_voice_e2ee_key', {
        channelId: target.channelId,
        userId: target.userId,
        counterpartyUserId: target.counterpartyUserId,
      });
      keyProvider = new ExternalE2EEKeyProvider();
      // setKey expects ArrayBuffer; .buffer is the underlying allocation.
      await keyProvider.setKey(new Uint8Array(keyInfo.key).buffer);
      console.info('[livekit-view] e2ee armed', {
        mls_group: keyInfo.mls_group_id,
        epoch: keyInfo.epoch,
        key_index: keyInfo.key_index,
      });
      this.currentKeyProvider = keyProvider;
      this.currentE2eeMlsGroupId = keyInfo.mls_group_id;
    } catch (e) {
      console.warn(
        '[livekit-view] e2ee key fetch failed — screen-share will NOT be end-to-end encrypted:',
        e,
      );
      keyProvider = null;
    }

    const room = new Room({
      adaptiveStream: true,
      dynacast: true,
      ...(keyProvider != null
        ? {
            e2ee: {
              keyProvider,
              // livekit-client ships an E2EE worker; the URL form lets
              // Vite resolve it from the package's exports without
              // bundling it into the main chunk.
              worker: new Worker(
                new URL('livekit-client/e2ee-worker', import.meta.url),
                { type: 'module' },
              ),
            },
          }
        : {}),
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
      if (keyProvider) {
        await room.setE2EEEnabled(true);
      }
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

  /** Rotate the E2EE key on the active screen-share view client. Called
   *  from VoiceSessionManager when the Rust side emits
   *  `voice_e2ee_key_rotated` after an MLS commit. No-op if no view
   *  client is connected or if the rotation is for a different group. */
  async rotateE2eeKey(
    key: Uint8Array,
    mlsGroupId: string,
  ): Promise<void> {
    const provider = this.currentKeyProvider;
    const activeGroup = this.currentE2eeMlsGroupId;
    if (!provider || activeGroup !== mlsGroupId) {
      return;
    }
    try {
      // Copy into a fresh ArrayBuffer (the source may be a view into a
      // larger buffer or a SharedArrayBuffer); setKey wants ArrayBuffer.
      const buf = new ArrayBuffer(key.byteLength);
      new Uint8Array(buf).set(key);
      await provider.setKey(buf);
      console.info('[livekit-view] e2ee key rotated', {
        mls_group: mlsGroupId,
      });
    } catch (e) {
      console.warn('[livekit-view] e2ee key rotation failed:', e);
    }
  }

  private async executeLeave(): Promise<void> {
    const room = this.currentRoom;
    this.currentRoom = null;
    this.current = null;
    this.currentKeyProvider = null;
    this.currentE2eeMlsGroupId = null;
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
    // Unconditional share reset on leave. The union structure means
    // share state lives inside `joined.share`, so once voiceLeft() lands
    // it's gone too — but call shareStopped() first to be explicit while
    // we're still in `joined`, in case the consumer flow handles
    // share-stopped and voice-left as distinct UI events.
    appStore.shareStopped();
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
      this.currentKeyProvider = null;
      this.currentE2eeMlsGroupId = null;
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
    const store = appStore;
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
      store.setScreenShareRemotes(desired);
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
    const s = appStore;
    if (s.voiceState.kind !== 'joined') {
      return null;
    }
    if (!s.currentUser) {
      return null;
    }
    return {
      channelId: s.voiceState.channelId,
      userId: s.currentUser.id,
      displayName: s.currentUser.username ?? s.currentUser.id,
      counterpartyUserId: s.voiceState.counterpartyUserId,
    };
  };

  // `autorun` applies once immediately (covering the case where voice was
  // already joined when this file is imported) and re-runs whenever the voice
  // state or current user that `computeIntent` reads changes.
  autorun(() => {
    livekitView.setIntent(computeIntent());
  });
}
