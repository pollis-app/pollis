// Seeds 40 security events for the signed-in Maestro account (#1548), so the
// Security events screen has more than one page to scroll and every filter has
// rows. Runs on the HOST (Maestro's JS engine), against the LOCAL DS's sqld —
// a client has no database credential and the DS has no "write me 40 events"
// endpoint, so this is the only way to get a deterministic, multi-page log.
//
// Env: SQLD_URL (e.g. http://127.0.0.1:8090 — the sqld behind a local DS) and
// EMAIL (the account `sign-in.yaml` just created). With no SQLD_URL (a run
// against the dev DS) nothing is written and `output.securitySeeded` is false;
// the flow then checks only what an unseeded account can show.
//
// The rows are dated 2099 so they sort ABOVE the account's real sign-up events
// and the newest three are known: mae-039, mae-038, mae-037. Kinds cycle by
// index % 5: device_enrolled, device_revoked, email_changed, identity_rotated,
// recovery_blob_fetched.

var url = typeof SQLD_URL === "string" ? SQLD_URL : "";
output.securitySeeded = false;
if (url.indexOf("http") === 0) {
  var kinds = [
    "device_enrolled",
    "device_revoked",
    "email_changed",
    "identity_rotated",
    "recovery_blob_fetched",
  ];
  // The ids are fixed (the flow asserts on them), so clear a previous run's
  // rows first; `id` is the table's primary key.
  var requests = [
    { type: "execute", stmt: { sql: "DELETE FROM security_event WHERE id LIKE 'mae-%'" } },
  ];
  for (var i = 0; i < 40; i++) {
    var n = ("00" + i).slice(-3);
    var kind = kinds[i % 5];
    var metadata =
      kind === "email_changed"
        ? { type: "text", value: '{"from":"old@example.com","to":"new@example.com"}' }
        : { type: "null" };
    requests.push({
      type: "execute",
      stmt: {
        sql:
          "INSERT INTO security_event (id, user_id, kind, device_id, created_at, metadata) " +
          "SELECT ?, id, ?, NULL, ?, ? FROM users WHERE lower(email) = lower(?)",
        args: [
          { type: "text", value: "mae-" + n },
          { type: "text", value: kind },
          // Two rows per second: page boundaries fall inside equal
          // timestamps, which only the id tiebreak keeps straight.
          { type: "text", value: "2099-01-01 00:00:" + ("0" + Math.floor(i / 2)).slice(-2) },
          metadata,
          { type: "text", value: EMAIL },
        ],
      },
    });
  }
  requests.push({ type: "close" });
  var resp = http.post(url.replace(/\/$/, "") + "/v2/pipeline", {
    headers: { "Content-Type": "application/json", Authorization: "Bearer local" },
    body: JSON.stringify({ requests: requests }),
  });
  if (resp.status !== 200) {
    throw new Error("seeding security events failed: " + resp.status + " " + resp.body);
  }
  var results = json(resp.body).results;
  var written = 0;
  for (var j = 0; j < results.length - 1; j++) {
    if (results[j].type !== "ok") {
      throw new Error("seeding security events failed: " + JSON.stringify(results[j]));
    }
    // results[0] is the DELETE.
    if (j > 0) {
      written += results[j].response.result.affected_row_count;
    }
  }
  if (written !== 40) {
    throw new Error("seeded " + written + " security events, expected 40 (no users row for " + EMAIL + "?)");
  }
  output.securitySeeded = true;
}
