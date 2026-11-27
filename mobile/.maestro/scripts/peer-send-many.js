// Asks the live-message peer (pollis-tui `live_message_peer`) to send COUNT messages into its DM,
// "<PREFIX> 01", "<PREFIX> 02", … in order (#2125). Returns once the DS has the last one.

var count = parseInt(COUNT, 10);
for (var i = 1; i <= count; i++) {
  var text = PREFIX + " " + (i < 10 ? "0" + i : "" + i);
  var response = http.get(
    "http://127.0.0.1:" + PEER_PORT + "/send?text=" + encodeURIComponent(text),
  );
  if (!response.ok) {
    throw new Error("peer send failed: " + response.status + " " + response.body);
  }
}
