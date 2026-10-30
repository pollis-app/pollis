// Asks the live-message peer (pollis-tui `live_message_peer`, #1610) to send TEXT into its DM.
// Returns once the DS has the message, so the flow's next wait measures DS -> screen.

var response = http.get(
  "http://127.0.0.1:" + PEER_PORT + "/send?text=" + encodeURIComponent(TEXT),
);
if (!response.ok) {
  throw new Error("peer send failed: " + response.status + " " + response.body);
}
output.sentAt = response.body;
