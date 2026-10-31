// Drives the typing peer (pollis-tui `typing_peer`, #1635) over loopback HTTP.
//   ACTION=on|off  the peer starts (re-sending every 3 s, as desktop does) or stops typing
//   ACTION=seen    output.seen = what the phone last published: typing | stopped | none
// For `seen`, EXPECT makes the script poll for up to 10 s and throw if it never matches.

var base = "http://127.0.0.1:" + PEER_PORT;

if (ACTION === "on" || ACTION === "off") {
  var r = http.get(base + "/typing?on=" + (ACTION === "on" ? "1" : "0"));
  if (!r.ok) {
    throw new Error("peer typing " + ACTION + " failed: " + r.status + " " + r.body);
  }
} else if (ACTION === "seen") {
  var want = typeof EXPECT === "undefined" ? null : EXPECT;
  var seen = null;
  var deadline = Date.now() + 10000;
  do {
    var s = http.get(base + "/seen");
    if (!s.ok) {
      throw new Error("peer seen failed: " + s.status);
    }
    seen = s.body;
    if (!want || seen === want) {
      break;
    }
    // Maestro's JS has no sleep; a short busy wait between polls.
    var until = Date.now() + 250;
    while (Date.now() < until) {}
  } while (Date.now() < deadline);
  output.seen = seen;
  if (want && seen !== want) {
    throw new Error("the peer saw '" + seen + "', expected '" + want + "'");
  }
} else {
  throw new Error("unknown ACTION " + ACTION);
}
