// Drives the typing peer's presence side (pollis-tui `typing_peer`, #2110) over loopback HTTP.
//   ACTION=hide|show  the peer hides or shows its window, as desktop's shell does: away / online
//   ACTION=seen       output.presence = the phone's presence as the peer sees it: online | away | offline
// For `seen`, EXPECT makes the script poll for up to 30 s and throw if it never matches.

var base = "http://127.0.0.1:" + PEER_PORT;

if (ACTION === "hide" || ACTION === "show") {
  var r = http.get(base + "/" + ACTION);
  if (!r.ok) {
    throw new Error("peer " + ACTION + " failed: " + r.status + " " + r.body);
  }
} else if (ACTION === "seen") {
  var want = typeof EXPECT === "undefined" ? null : EXPECT;
  var seen = null;
  var deadline = Date.now() + 30000;
  do {
    var s = http.get(base + "/presence");
    if (!s.ok) {
      throw new Error("peer presence failed: " + s.status);
    }
    seen = s.body;
    if (!want || seen === want) {
      break;
    }
    // Maestro's JS has no sleep; a short busy wait between polls.
    var until = Date.now() + 250;
    while (Date.now() < until) {}
  } while (Date.now() < deadline);
  output.presence = seen;
  if (want && seen !== want) {
    throw new Error("the peer sees the phone '" + seen + "', expected '" + want + "'");
  }
} else {
  throw new Error("unknown ACTION " + ACTION);
}
