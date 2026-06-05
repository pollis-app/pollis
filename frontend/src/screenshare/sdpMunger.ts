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
  // transceiver list and pin preferences on any new SEND-capable video
  // transceivers we may have missed.
  //
  // Recv-only transceivers are intentionally skipped: the AV1 stripper
  // exists to keep AV1 out of OUR outgoing offer / answer. A recv-only
  // transceiver doesn't generate the SDP we send and pinning codec
  // preferences on one with E2EE enabled fights Chromium's per-
  // transceiver codec restrictions, generating spurious
  //   "Missing codec from recv codec capabilities" /
  //   "invalid codec with name H264"
  // errors with no behavior change. Codec collisions in the incoming
  // OFFER (BUNDLE PT collisions) come from the SFU and aren't
  // fixable from setCodecPreferences; Chromium logs them and proceeds.
  const origSetRemote = proto.setRemoteDescription;
  proto.setRemoteDescription = async function (
    this: RTCPeerConnection,
    desc: RTCSessionDescriptionInit,
  ): Promise<void> {
    const result = origSetRemote.call(this, desc);
    await result;
    for (const t of this.getTransceivers()) {
      const kind = t.receiver?.track?.kind ?? t.sender?.track?.kind;
      if (kind !== "video") {
        continue;
      }
      const isRecvOnly =
        t.currentDirection === "recvonly" || t.direction === "recvonly";
      if (isRecvOnly) {
        continue;
      }
      pinNonAv1Preferences(t, "video");
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
    if (kind !== "video") {
      return;
    }
    // A recv-only transceiver's setCodecPreferences validates each codec
    // against the receiver's supported codec set (Codec::Matches in
    // libwebrtc), which compares H.264 by profile-level-id. Passing
    // sender-only H.264 entries (e.g. VideoToolbox's high-profile
    // 640034) to a recv-only transceiver triggers
    //   "Invalid codec preferences: invalid codec with name "H264"."
    // Use receiver caps for recv-only transceivers, sender caps
    // otherwise.
    const isRecvOnly =
      transceiver.currentDirection === "recvonly" ||
      transceiver.direction === "recvonly";
    const caps = isRecvOnly
      ? RTCRtpReceiver.getCapabilities?.("video")
      : RTCRtpSender.getCapabilities?.("video");
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
    try {
      transceiver.setCodecPreferences(filtered);
    } catch (e) {
      // First attempt rejected — fall through to per-codec narrowing below.
      console.warn(
        "[av1-stripper] full setCodecPreferences rejected, narrowing:",
        {
          isRecvOnly,
          direction: transceiver.direction,
          currentDirection: transceiver.currentDirection,
          mid: transceiver.mid,
          filtered: filtered.map(
            (c) => `${c.mimeType}|${c.sdpFmtpLine ?? ""}`,
          ),
          err: (e as Error).message,
        },
      );
      // Try each codec individually to find the offender(s); keep what's
      // accepted, drop what isn't. We use this narrowed set on a second
      // attempt so the AV1 filter still applies as much as the transceiver
      // will allow.
      const accepted: RTCRtpCodec[] = [];
      const rejected: string[] = [];
      for (const codec of filtered) {
        try {
          transceiver.setCodecPreferences([codec]);
          accepted.push(codec);
        } catch {
          rejected.push(`${codec.mimeType}|${codec.sdpFmtpLine ?? ""}`);
        }
      }
      console.warn("[av1-stripper] narrowing result:", {
        accepted: accepted.map(
          (c) => `${c.mimeType}|${c.sdpFmtpLine ?? ""}`,
        ),
        rejected,
      });
      if (accepted.length > 0) {
        try {
          transceiver.setCodecPreferences(accepted);
        } catch (e2) {
          console.warn(
            "[av1-stripper] narrowed setCodecPreferences also rejected:",
            e2,
          );
        }
      }
    }
  } catch (e) {
    console.warn("[av1-stripper] pinNonAv1Preferences failed:", e);
  }
}
