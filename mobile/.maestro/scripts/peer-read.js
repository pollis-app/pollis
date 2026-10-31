// Asks the live-message peer (pollis-tui `live_message_peer`) to fetch its DM and mark the
// device's messages read (#1637). Returns once the read receipt is on the DS.

var response = http.get("http://127.0.0.1:" + PEER_PORT + "/read");
if (!response.ok) {
  throw new Error("peer read failed: " + response.status + " " + response.body);
}
output.readCount = response.body;
