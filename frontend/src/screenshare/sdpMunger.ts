// Chromium 130+ AV1 BUNDLE / RTX strip workaround.
//
// Symptom: when livekit-client publishes a screen-share video track,
// Chromium's RTCPeerConnection emits an offer that its own
// `sdp_offer_answer.cc` validator refuses to apply with one of:
//
//   A BUNDLE group contains a codec collision for payload_type='35'.
//   RTX codec (PT=46) mapped to PT=45 which is not in the codec list.
//   GetChangedReceiverParameters called without any video codecs.
//   Failed to set local video description recv parameters for m-section.
//
// All three are downstream of the same root cause: Chromium 130 unconditionally
// offers AV1 (`video/AV1`) plus an associated RTX retransmission codec in
// screen-share contexts, then fails its own validation on the resulting
// offer. Not a livekit-client bug — it's the offer Chromium generated.
//
// Workaround: monkey-patch `RTCPeerConnection.prototype.setLocalDescription`
// to strip AV1 *and* its associated RTX entries from the offer before
// Chromium validates it. AV1 in screen-share isn't worth the CPU cost on
// most hardware anyway; VP8 + H.264 + VP9 cover every reasonable receiver.
//
// Three steps, each non-trivial:
//   1. Find every AV1 PT (from `a=rtpmap:<PT> AV1/...`).
//   2. Find every RTX PT that maps to an AV1 PT (`a=fmtp:<RTX_PT> apt=<AV1_PT>`).
//      Drop those too — leaving them creates the "RTX mapped to PT not in
//      codec list" error.
//   3. Remove all rtpmap/fmtp/rtcp-fb lines for the dropped PTs, and prune
//      the PTs from each `m=video` header. If an m=video section ends up
//      with zero codecs, mark the m-section as rejected (`port=0`) per
//      RFC 5888 — otherwise Chromium throws "called without any video
//      codecs".

const RTPMAP_RE = /^a=rtpmap:(\d+) ([^\/]+)\//;
const FMTP_RE = /^a=fmtp:(\d+) (.+)$/;

interface CodecMap {
  /** PT → codec name (uppercase, e.g. "AV1", "VP8", "RTX"). */
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

function stripAv1(sdp: string): string {
  const { ptToCodec, rtxApt } = parseCodecs(sdp);

  // Step 1: AV1 PTs.
  const droppedPts = new Set<string>();
  for (const [pt, codec] of ptToCodec) {
    if (codec === "AV1") {
      droppedPts.add(pt);
    }
  }
  if (droppedPts.size === 0) {
    return sdp;
  }

  // Step 2: RTX PTs whose `apt=` references a dropped AV1 PT.
  for (const [rtxPt, aptTarget] of rtxApt) {
    if (droppedPts.has(aptTarget)) {
      droppedPts.add(rtxPt);
    }
  }

  // Step 3: rewrite the SDP.
  const lines = sdp.split("\r\n");
  const out: string[] = [];
  for (const line of lines) {
    // Drop rtpmap / fmtp / rtcp-fb lines for any dropped PT.
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

    // Update `m=video <port> <proto> <pt1> <pt2> ...` — strip dropped PTs.
    // If zero PTs remain, set port=0 to reject the m-section per RFC 5888 —
    // Chromium otherwise throws "GetChangedReceiverParameters called
    // without any video codecs".
    if (line.startsWith("m=video ")) {
      const parts = line.split(" ");
      const head = parts.slice(0, 3); // m=<media> <port> <proto>
      const tail = parts.slice(3).filter((pt) => !droppedPts.has(pt));
      if (tail.length === 0) {
        // Reject the m-section.
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
  const proto = RTCPeerConnection.prototype as unknown as {
    setLocalDescription: (
      desc?: RTCLocalSessionDescriptionInit,
    ) => Promise<void>;
  };
  const original = proto.setLocalDescription;
  proto.setLocalDescription = function (
    desc?: RTCLocalSessionDescriptionInit,
  ): Promise<void> {
    if (desc?.sdp) {
      const munged: RTCLocalSessionDescriptionInit = {
        type: desc.type,
        sdp: stripAv1(desc.sdp),
      };
      return original.call(this, munged);
    }
    return original.call(this, desc);
  };
}

// Exported for unit testing / debugging only.
export const __test__ = { stripAv1, parseCodecs };
