// Chromium 130+ AV1 PT=35 BUNDLE collision workaround — minimal version.
//
// Earlier iterations of this file munged setLocalDescription/setRemoteDescription
// SDP strings to strip AV1. That worked in isolation but desynchronised
// the SDP state between Chromium (munged) and livekit-client (unmunged,
// since livekit-client keeps its own copy of the offer it generates and
// sends *that* to the LiveKit server). The server's resulting answer was
// negotiated against AV1, our local PC was committed to non-AV1, and
// publishTrack's "wait for server publication ACK" promise hung forever.
//
// The standards-track fix is `RTCRtpTransceiver.setCodecPreferences` —
// it filters AV1 out of the codec list BEFORE Chromium generates the
// offer, so the SDP everybody sees (Chromium local, livekit-client
// sent, LiveKit server received) is consistent and AV1-free. No
// SDP-string munging needed downstream.
//
// We patch addTransceiver instead of every individual publishTrack site
// because livekit-client funnels track addition through addTransceiver
// internally (via addTrack and direct addTransceiver calls). One patch,
// every video transceiver covered.

const DROPPED_CODECS = new Set(["AV1"]);

let installed = false;

// When set, video transceivers pin this exact codec capability FIRST in
// their preference list (after AV1 stripping) so SDP negotiation selects
// it. Used to force the hardware H.264 high-profile entry for screen-share
// publish (see codecPolicy.ts / issue #364). Null → default Chromium order
// (VP8-first), the software fallback. It's a module-level flag rather than
// a per-call argument because livekit-client funnels every track through
// the patched addTransceiver internally — the publish site sets the flag,
// then the patch reads it when the transceiver is created.
let preferredVideoCodec: RTCRtpCodec | null = null;

/** Set (or clear, with null) the video codec to pin first on subsequent
 *  video transceiver creation / renegotiation. */
export function setPreferredVideoCodec(
  codec: RTCRtpCodec | null,
): void {
  preferredVideoCodec = codec;
}

export function installAv1Stripper(): void {
  if (installed || typeof RTCPeerConnection === "undefined") {
    return;
  }
  installed = true;
  console.info(
    "[av1-stripper] installed — filtering AV1 from video transceiver codec preferences",
  );

  const proto = RTCPeerConnection.prototype as unknown as {
    addTransceiver: typeof RTCPeerConnection.prototype.addTransceiver;
    setRemoteDescription: (desc: RTCSessionDescriptionInit) => Promise<void>;
  };

  const origAddTransceiver = proto.addTransceiver;
  proto.addTransceiver = function (
    this: RTCPeerConnection,
    trackOrKind: MediaStreamTrack | string,
    init?: RTCRtpTransceiverInit,
  ): RTCRtpTransceiver {
    const transceiver = origAddTransceiver.call(this, trackOrKind, init);
    pinNonAv1Preferences(transceiver, trackOrKind);
    return transceiver;
  };

  // Receive-only m-sections (subscribed remote tracks) get implicit
  // transceivers created by setRemoteDescription, bypassing our
  // addTransceiver patch. After the remote desc lands, sweep the
  // transceiver list and pin preferences on any new video transceivers.
  const origSetRemote = proto.setRemoteDescription;
  proto.setRemoteDescription = async function (
    this: RTCPeerConnection,
    desc: RTCSessionDescriptionInit,
  ): Promise<void> {
    const result = origSetRemote.call(this, desc);
    await result;
    for (const t of this.getTransceivers()) {
      const kind = t.receiver?.track?.kind ?? t.sender?.track?.kind;
      if (kind === "video") {
        pinNonAv1Preferences(t, "video");
      }
    }
  };
}

function pinNonAv1Preferences(
  transceiver: RTCRtpTransceiver,
  trackOrKind: MediaStreamTrack | string,
): void {
  try {
    const kind =
      typeof trackOrKind === "string" ? trackOrKind : trackOrKind.kind;
    if (kind !== "video" || typeof RTCRtpSender === "undefined") {
      return;
    }
    const caps = RTCRtpSender.getCapabilities?.("video");
    if (!caps?.codecs) {
      return;
    }
    const filtered = caps.codecs.filter((c) => {
      const codecName = c.mimeType.split("/")[1]?.toUpperCase();
      return codecName ? !DROPPED_CODECS.has(codecName) : true;
    });
    if (filtered.length === 0) {
      return; // never call setCodecPreferences([]) — spec-invalid
    }
    transceiver.setCodecPreferences(reorderPreferred(filtered));
  } catch (e) {
    console.warn("[av1-stripper] setCodecPreferences failed:", e);
  }
}

/** Move the pinned `preferredVideoCodec` (matched by mimeType + fmtp line)
 *  to the front of an already-filtered codec list so SDP negotiation picks
 *  it. No-op when nothing is pinned or the pinned codec isn't present in
 *  this PC's capabilities. */
function reorderPreferred(
  codecs: RTCRtpCodec[],
): RTCRtpCodec[] {
  const preferred = preferredVideoCodec;
  if (!preferred) {
    return codecs;
  }
  const matches = (c: RTCRtpCodec): boolean =>
    c.mimeType.toLowerCase() === preferred.mimeType.toLowerCase() &&
    (c.sdpFmtpLine ?? "") === (preferred.sdpFmtpLine ?? "");
  const head = codecs.filter(matches);
  if (head.length === 0) {
    return codecs;
  }
  const tail = codecs.filter((c) => !matches(c));
  return [...head, ...tail];
}
