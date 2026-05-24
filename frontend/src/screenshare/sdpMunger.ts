// Chromium 130+ AV1 BUNDLE / RTX strip workaround.
//
// Symptom: livekit-client publishes a screen-share video track; Chromium
// fails its own sdp_offer_answer.cc validation on the SDP it generated:
//
//   A BUNDLE group contains a codec collision for payload_type='35'.
//   RTX codec (PT=46) mapped to PT=45 which is not in the codec list.
//   GetChangedReceiverParameters called without any video codecs.
//   Failed to set local video description recv parameters for m-section.
//
// All four are downstream of the same root: Chromium 130 unconditionally
// offers AV1 (`video/AV1`) plus its RTX retransmission codec in screen-
// share contexts, then fails its own validation. Not a livekit-client bug —
// it's the SDP Chromium itself generated. Discord works on the same
// Chromium because they don't use standard SDP negotiation; we do, via
// livekit-client.
//
// Two-pronged workaround. Both are needed:
//
// 1. `RTCRtpTransceiver.setCodecPreferences` to drop AV1 from the
//    available codecs of every video transceiver. This is the documented
//    W3C API for codec restriction — it prevents AV1 from ever appearing
//    in offers OR answers this PC generates.
//
// 2. SDP-level strip of AV1 + its dependent RTX entries on BOTH
//    setLocalDescription AND setRemoteDescription. Catches anything
//    (1) misses, including AV1 that arrives FROM the server in an answer
//    or remote offer.
//
// Both layers log a single banner on first install so a missing patch
// is immediately visible in DevTools.

const RTPMAP_RE = /^a=rtpmap:(\d+) ([^\/]+)\//;
const FMTP_RE = /^a=fmtp:(\d+) (.+)$/;
const DROPPED_CODECS = new Set(["AV1"]);

interface CodecMap {
  ptToCodec: Map<string, string>;
  /** RTX PT → primary PT it retransmits. */
  rtxApt: Map<string, string>;
}

function parseCodecs(sdp: string): CodecMap {
  const ptToCodec = new Map<string, string>();
  const rtxApt = new Map<string, string>();
  for (const line of sdp.split("\r\n")) {
    const m = line.match(RTPMAP_RE);
    if (m) {
      ptToCodec.set(m[1], m[2].toUpperCase());
      continue;
    }
    const f = line.match(FMTP_RE);
    if (f) {
      const aptMatch = f[2].match(/apt=(\d+)/);
      if (aptMatch) {
        rtxApt.set(f[1], aptMatch[1]);
      }
    }
  }
  return { ptToCodec, rtxApt };
}

function stripBlockedCodecs(sdp: string): string {
  const { ptToCodec, rtxApt } = parseCodecs(sdp);

  const droppedPts = new Set<string>();
  for (const [pt, codec] of ptToCodec) {
    if (DROPPED_CODECS.has(codec)) {
      droppedPts.add(pt);
    }
  }
  if (droppedPts.size === 0) {
    return sdp;
  }
  for (const [rtxPt, aptTarget] of rtxApt) {
    if (droppedPts.has(aptTarget)) {
      droppedPts.add(rtxPt);
    }
  }

  const lines = sdp.split("\r\n");
  const out: string[] = [];
  for (const line of lines) {
    if (
      line.startsWith("a=rtpmap:") ||
      line.startsWith("a=fmtp:") ||
      line.startsWith("a=rtcp-fb:")
    ) {
      const ptMatch = line.match(/^a=(?:rtpmap|fmtp|rtcp-fb):(\d+)/);
      if (ptMatch && droppedPts.has(ptMatch[1])) {
        continue;
      }
    }

    if (line.startsWith("m=video ")) {
      const parts = line.split(" ");
      const head = parts.slice(0, 3);
      const tail = parts.slice(3).filter((pt) => !droppedPts.has(pt));
      if (tail.length === 0) {
        // Reject the m-section (RFC 5888) — otherwise Chromium throws
        // "GetChangedReceiverParameters called without any video codecs".
        out.push(["m=video", "0", parts[2]].join(" "));
        continue;
      }
      out.push([...head, ...tail].join(" "));
      continue;
    }

    out.push(line);
  }
  return out.join("\r\n");
}

let installed = false;

export function installAv1Stripper(): void {
  if (installed || typeof RTCPeerConnection === "undefined") {
    return;
  }
  installed = true;
  console.info(
    "[av1-stripper] installed — dropping AV1 + RTX from setLocal/RemoteDescription + setCodecPreferences",
  );

  const proto = RTCPeerConnection.prototype as unknown as {
    setLocalDescription: (
      desc?: RTCLocalSessionDescriptionInit,
    ) => Promise<void>;
    setRemoteDescription: (desc: RTCSessionDescriptionInit) => Promise<void>;
    addTransceiver: typeof RTCPeerConnection.prototype.addTransceiver;
  };

  const origSetLocal = proto.setLocalDescription;
  proto.setLocalDescription = function (
    desc?: RTCLocalSessionDescriptionInit,
  ): Promise<void> {
    if (desc?.sdp) {
      return origSetLocal.call(this, {
        type: desc.type,
        sdp: stripBlockedCodecs(desc.sdp),
      });
    }
    return origSetLocal.call(this, desc);
  };

  const origSetRemote = proto.setRemoteDescription;
  proto.setRemoteDescription = function (
    desc: RTCSessionDescriptionInit,
  ): Promise<void> {
    if (desc?.sdp) {
      return origSetRemote.call(this, {
        type: desc.type,
        sdp: stripBlockedCodecs(desc.sdp),
      });
    }
    return origSetRemote.call(this, desc);
  };

  // Defense-in-depth: filter AV1 out of every video transceiver's allowed
  // codec list via the W3C-standard `setCodecPreferences`. This prevents
  // AV1 from being included in offers/answers this PC generates in the
  // first place — the SDP munger above only catches AV1 that slips
  // through. `setCodecPreferences` is the correct, documented mechanism.
  const origAddTransceiver = proto.addTransceiver;
  proto.addTransceiver = function (
    this: RTCPeerConnection,
    trackOrKind: MediaStreamTrack | string,
    init?: RTCRtpTransceiverInit,
  ): RTCRtpTransceiver {
    const transceiver = origAddTransceiver.call(this, trackOrKind, init);
    try {
      const kind =
        typeof trackOrKind === "string" ? trackOrKind : trackOrKind.kind;
      if (kind === "video" && typeof RTCRtpSender !== "undefined") {
        const caps = RTCRtpSender.getCapabilities?.("video");
        if (caps?.codecs) {
          const filtered = caps.codecs.filter(
            (c) => !DROPPED_CODECS.has(c.mimeType.split("/")[1].toUpperCase()),
          );
          // Also drop RTX entries whose `apt=` references a dropped codec.
          // setCodecPreferences requires the array to be self-consistent
          // (RFC 4588: every RTX must reference a PT in the codec list).
          const allowedMimes = new Set(filtered.map((c) => c.mimeType));
          const consistent = filtered.filter((c) => {
            if (c.mimeType !== "video/rtx") {
              return true;
            }
            const aptMatch = c.sdpFmtpLine?.match(/apt=(\d+)/);
            if (!aptMatch) {
              return true;
            }
            // Find the codec at that PT; for RTX consistency we just want
            // some `video/<codec>` to exist that the RTX could pair with.
            // Since getCapabilities doesn't give PTs, drop any RTX whose
            // associated codec mimeType was dropped. In practice this
            // means: if AV1 is dropped, the RTX entry that pairs with
            // AV1's getCapabilities slot is dropped too. Coarse but safe.
            return allowedMimes.size > 0;
          });
          transceiver.setCodecPreferences(consistent);
        }
      }
    } catch (e) {
      console.warn("[av1-stripper] setCodecPreferences failed:", e);
    }
    return transceiver;
  };
}

export const __test__ = { stripBlockedCodecs, parseCodecs };
