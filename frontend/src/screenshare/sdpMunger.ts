// Chromium 130 PT=35 BUNDLE collision workaround.
//
// Symptom: when livekit-client (or any WebRTC code) publishes a video
// track, `setLocalDescription` fails with:
//   ERROR:sdp_offer_answer.cc(424) A BUNDLE group contains a codec
//   collision for payload_type='35'. All codecs must share the same type,
//   encoding name, clock rate and parameters. (INVALID_PARAMETER)
//
// Cause: Chromium's PT allocator hands payload_type 35 to AV1
// (`video/AV1`) AND to another video codec in the same offer, and then
// fails its own BUNDLE invariant check. Known Chromium bug surfaced by
// the screen-share publish path; no clean app-level config knob disables
// AV1 on a per-PC basis.
//
// Fix: monkey-patch `RTCPeerConnection.setLocalDescription` to strip AV1
// from the offer SDP before Chromium validates it. AV1 in screen share is
// not worth the CPU cost on most hardware anyway — VP8/H.264 cover all
// reasonable subscribers. Targeted: only video m= lines, only AV1's
// rtpmap/fmtp/rtcp-fb lines + the AV1 PT in the m= line itself.

const AV1_RE = /^a=rtpmap:(\d+) AV1\//i;

function av1PayloadTypes(sdp: string): Set<string> {
  const pts = new Set<string>();
  for (const line of sdp.split("\r\n")) {
    const m = line.match(AV1_RE);
    if (m) {
      pts.add(m[1]);
    }
  }
  return pts;
}

function stripAv1(sdp: string): string {
  const pts = av1PayloadTypes(sdp);
  if (pts.size === 0) {
    return sdp;
  }
  const lines = sdp.split("\r\n");
  const out: string[] = [];
  for (const line of lines) {
    if (line.startsWith("a=rtpmap:") || line.startsWith("a=fmtp:") || line.startsWith("a=rtcp-fb:")) {
      const ptMatch = line.match(/^a=(?:rtpmap|fmtp|rtcp-fb):(\d+)/);
      if (ptMatch && pts.has(ptMatch[1])) {
        continue;
      }
    }
    if (line.startsWith("m=video ")) {
      const parts = line.split(" ");
      // m=<media> <port> <proto> <fmt> ...
      const head = parts.slice(0, 3);
      const tail = parts.slice(3).filter((pt) => !pts.has(pt));
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
