// Screen-share codec policy for the Electron (Chromium) publish path.
//
// The hardware-H.264 work in #293/#296 landed on the Rust screenshare
// path, which the Electron build bypasses entirely (capture + encode +
// publish all happen in Chromium). This module restores hardware H.264 on
// the browser path, where Chromium already ships and maintains the
// per-platform hardware-encoder glue (VideoToolbox / Media Foundation /
// VAAPI) — we don't compile or babysit any of it. See issue #364.
//
// Decision, made per-machine at publish time:
//   - If a hardware H.264 encoder is present → publish H.264 pinned to its
//     advertised high level, so SDP negotiation never caps resolution /
//     framerate ("uncap the negotiation"). VideoToolbox/MF advertise a
//     High profile / Level 5.2 entry (`profile-level-id=640034`) that
//     covers 1080p60 and beyond.
//   - Otherwise → fall back to software VP8. SW VP8 has no level cap and
//     we control its bitrate/framerate, so it is a strictly better
//     fallback than the only SW H.264 on offer (OpenH264 baseline 3.1,
//     which can't do 1080p).
//
// The level test is the whole trick: OpenH264 (software) advertises ONLY
// baseline Level 3.1 (`profile-level-id` ending in `1f`). So the presence
// of any H.264 entry whose level is *higher* than 3.1 is itself proof a
// hardware encoder is registered.

export type ScreenShareCodec = 'h264' | 'vp8';

/** Env override for A/B testing. `VITE_POLLIS_SCREENSHARE_CODEC` = `h264`
 *  | `vp8` forces that codec regardless of capability detection. Anything
 *  else (including unset) → auto-detect. Mirrors the `POLLIS_SCREENSHARE_CODEC`
 *  override the Rust path used; on the browser path Vite only exposes
 *  `VITE_`-prefixed vars to the renderer. */
function codecOverride(): ScreenShareCodec | null {
  const v = import.meta.env.VITE_POLLIS_SCREENSHARE_CODEC;
  if (v === 'h264' || v === 'vp8') {
    return v;
  }
  return null;
}

/** Scan video sender capabilities for an H.264 entry whose level is above
 *  the software baseline (3.1). Returns the matching capability so the
 *  caller can pin it first via setCodecPreferences; null if none (only
 *  baseline `…1f` entries, i.e. software-only → use VP8). */
export function findHardwareH264(): RTCRtpCodec | null {
  if (typeof RTCRtpSender === 'undefined' || !RTCRtpSender.getCapabilities) {
    return null;
  }
  const caps = RTCRtpSender.getCapabilities('video');
  if (!caps?.codecs) {
    return null;
  }
  for (const codec of caps.codecs) {
    if (codec.mimeType.toLowerCase() !== 'video/h264') {
      continue;
    }
    const fmtp = codec.sdpFmtpLine ?? '';
    const match = /profile-level-id=([0-9a-fA-F]{6})/.exec(fmtp);
    if (!match) {
      continue;
    }
    // profile-level-id is profile_idc(2) + profile-iop(2) + level_idc(2).
    // level_idc 0x1f == Level 3.1, the software OpenH264 ceiling. Any
    // entry with a higher level means a hardware encoder is present.
    const levelId = match[1].toLowerCase();
    if (!levelId.endsWith('1f')) {
      return codec;
    }
  }
  return null;
}

/** Decide which codec to publish screen-share with on this machine.
 *  Honors the env override first, then falls back to hardware detection. */
export function pickScreenShareCodec(): {
  codec: ScreenShareCodec;
  /** The exact H.264 capability to pin first, when `codec === 'h264'`. */
  h264Capability: RTCRtpCodec | null;
} {
  const override = codecOverride();
  const hw = findHardwareH264();
  if (override === 'vp8') {
    return { codec: 'vp8', h264Capability: null };
  }
  if (override === 'h264') {
    return { codec: 'h264', h264Capability: hw };
  }
  if (hw) {
    return { codec: 'h264', h264Capability: hw };
  }
  return { codec: 'vp8', h264Capability: null };
}
