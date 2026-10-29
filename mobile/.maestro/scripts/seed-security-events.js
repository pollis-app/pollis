// Seeds 40 security events (#1548) via the LOCAL DS's sqld (SQLD_URL); dated 2099 so they sort first. No SQLD_URL: nothing written.

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
  // The ids are fixed (the flow asserts on them): clear a previous run's rows.
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
          // Two rows per second: only the id tiebreak orders page boundaries.
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
