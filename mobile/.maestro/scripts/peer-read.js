// Asks the live-message peer (pollis-tui `live_message_peer`) to fetch its DM and mark the
// device's messages read (#1637). The peer first waits until it holds EXPECT of them: the row
// shows the moment Send is tapped, before the DS has the message, so reading straight away
// would race the send and leave the newest message unacknowledged. Returns once the read
// receipt is on the DS.

var response = http.get(
  "http://127.0.0.1:" + PEER_PORT + "/read?expect=" + encodeURIComponent(EXPECT),
);
if (!response.ok) {
  throw new Error("peer read failed: " + response.status + " " + response.body);
}
output.readCount = response.body;
