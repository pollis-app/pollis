// Renders the public status record on /status (#877).
//
// Sources: website/status-history.json (appended by scripts/status-probe.sh,
// run from status-probe.yml) and website/incidents.json (written by hand, in
// the format of docs/incidents/README.md, gated by scripts/check-incidents.py).
//
// Both are STATIC COMMITTED FILES, and that is deliberate rather than lazy: the
// DS sends no CORS headers, so a browser on pollis.com cannot fetch
// api.pollis.com/health at all. A page that tried would fail on every load and
// render a red alarm caused entirely by the browser's own origin policy — the
// #732 shape of mistake, where a stale or impossible client check tells every
// visitor they are under attack. So the probing happens out of band and the
// page renders what was recorded, with the recording time shown.

(function () {
  "use strict";

  var HISTORY_URL = "status-history.json";
  var INCIDENTS_URL = "incidents.json";

  // Two missed heartbeats. Past this, the page stops claiming to know anything:
  // a stale record that still renders green is the failure mode this whole page
  // exists to remove.
  var STALE_HOURS = 48;
  var MAX_ROWS = 40;

  function esc(s) {
    return String(s).replace(/[&<>"]/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
    });
  }

  function fmtTime(iso) {
    var d = new Date(iso);
    if (isNaN(d)) {
      return String(iso);
    }
    return d.toISOString().slice(0, 16).replace("T", " ") + " UTC";
  }

  function hoursSince(iso) {
    var d = new Date(iso);
    if (isNaN(d)) {
      return Infinity;
    }
    return (Date.now() - d.getTime()) / 3600000;
  }

  function badge(cls, text) {
    return '<span class="doc-badge doc-badge--' + cls + '">' + esc(text) + "</span>";
  }

  function shortSha(sha) {
    return sha ? String(sha).slice(0, 8) : "unknown";
  }

  // Is the transparency log actually being PUBLISHED? Not the same question as
  // "is a head being served" — an abandoned log serves the same bytes as a
  // healthy idle one, because a head's timestamp is frozen per tree size. Only
  // the publisher's run history separates them.
  function publishing(sample) {
    var p = (sample && sample.publisher) || {};
    if (!p.last_run_at) {
      return true;
    }
    return p.conclusion === "success" && hoursSince(p.last_run_at) < STALE_HOURS;
  }

  // ── current state ───────────────────────────────────────────────────────────
  function renderCurrent(data) {
    var el = document.getElementById("status-current");
    var el2 = document.getElementById("status-table");
    if (!el || !el2) {
      return;
    }
    var samples = data.samples || [];
    if (!samples.length) {
      el.innerHTML =
        '<p class="doc-loading">No observations have been recorded yet. The record opened on ' +
        esc(data.record_started_at || "an unrecorded date") +
        ".</p>";
      return;
    }

    var last = samples[samples.length - 1];
    var age = hoursSince(last.at);
    var stale = age > STALE_HOURS;
    var down = [];
    (data.targets || []).forEach(function (t) {
      var s = last.targets[t.id];
      if (s && !s.ok) {
        down.push(t.name);
      }
    });

    var headline;
    if (stale) {
      headline =
        badge("warn", "unknown") +
        " <strong>This record is " +
        Math.round(age) +
        " hours old, so it is out of date and the state below should not be " +
        "read as current.</strong> The prober itself has stopped recording — " +
        "that is a fault in the monitoring, not a statement about the services.";
    } else if (down.length) {
      headline =
        badge("fail", "degraded") +
        " <strong>" +
        esc(down.join(", ")) +
        " did not answer at the last observation.</strong> If an incident is " +
        "open it is listed below; if none is listed, one has not been written " +
        "up yet.";
    } else if (!publishing(last)) {
      // Deliberately part of the TOP LINE, not a detail further down. A page
      // that reports "all green" while the transparency log has stopped being
      // published would be reporting availability and calling it trust, which is
      // the thing section 4 argues against.
      headline =
        badge("warn", "degraded") +
        " <strong>Every probed endpoint answered, but the transparency log is not " +
        "currently being published.</strong> That is a correctness matter rather " +
        "than an outage &mdash; see below for what it does and does not mean.";
    } else {
      headline =
        badge("pass", "answering") +
        " <strong>Every probed endpoint answered at the last observation, and the " +
        "transparency log is being published.</strong>";
    }

    el.innerHTML =
      "<p>" +
      headline +
      "</p><p>Last observation <strong>" +
      esc(fmtTime(last.at)) +
      "</strong> (" +
      (age < 1 ? "under an hour ago" : Math.round(age) + " hours ago") +
      "). Running build <code>" +
      esc(shortSha(last.ds_sha)) +
      "</code>; latest release <code>" +
      esc(last.latest_release || "unknown") +
      "</code>.</p>";

    var rows = (data.targets || [])
      .map(function (t) {
        var s = last.targets[t.id] || {};
        var mark = s.ok
          ? badge("pass", "answered " + s.status)
          : badge("fail", s.status ? "HTTP " + s.status : "no answer");
        return (
          "<tr><td><strong>" +
          esc(t.name) +
          '</strong><br /><span class="doc-td-dim"><code>' +
          esc(t.url) +
          "</code></span></td><td>" +
          mark +
          '</td><td class="doc-td-dim">' +
          (s.ms != null ? esc(s.ms) + " ms" : "&mdash;") +
          '</td><td class="doc-td-dim">' +
          esc(t.proves) +
          "</td></tr>"
        );
      })
      .join("");

    el2.innerHTML =
      "<caption>Observed by <code>scripts/status-probe.sh</code> at " +
      esc(fmtTime(last.at)) +
      ", from a single GitHub-hosted runner.</caption>" +
      "<thead><tr><th>Service</th><th>Last observation</th><th>Response</th>" +
      "<th>What the check proves</th></tr></thead><tbody>" +
      rows +
      "</tbody>";
  }

  // ── the publisher, separately from the tree ─────────────────────────────────
  // A Signed Tree Head's timestamp is frozen per tree size, so an old timestamp
  // is what an IDLE log looks like AND what an ABANDONED one looks like. Only
  // the publishing workflow's own run history tells them apart, so it is
  // reported as its own line rather than folded into the log's row.
  function renderPublisher(data) {
    var el = document.getElementById("status-publisher");
    if (!el) {
      return;
    }
    var samples = data.samples || [];
    if (!samples.length) {
      return;
    }
    var p = samples[samples.length - 1].publisher || {};
    var trees = samples[samples.length - 1].trees || {};

    if (!p.last_run_at) {
      el.innerHTML =
        "<p>" +
        badge("info", "unknown") +
        " The last observation did not record the publisher's run history.</p>";
      return;
    }
    var age = hoursSince(p.last_run_at);
    var mark =
      p.conclusion === "success" && age < 48
        ? badge("pass", "published")
        : badge("warn", p.conclusion === "success" ? "overdue" : esc(p.conclusion));

    el.innerHTML =
      "<p>" +
      mark +
      " The transparency publisher last ran <strong>" +
      esc(fmtTime(p.last_run_at)) +
      "</strong> and concluded <code>" +
      esc(p.conclusion) +
      "</code>" +
      (p.run_url
        ? ' (<a href="' + esc(p.run_url) + '" target="_blank" rel="noopener noreferrer">run</a>)'
        : "") +
      ". It is scheduled daily, so a gap of more than about two days means it " +
      "has stopped, whatever the served heads look like.</p>" +
      "<p>Heads served at that observation: commit log <code>" +
      esc(trees.commit_log != null ? trees.commit_log : "?") +
      "</code> entries, account keys <code>" +
      esc(trees.account_keys != null ? trees.account_keys : "?") +
      "</code>, binaries <code>" +
      esc(trees.binaries != null ? trees.binaries : "?") +
      "</code>. A tree that stops growing is not by itself a fault — trees only " +
      "grow when there is something to publish.</p>";
  }

  // ── what changed between observations ───────────────────────────────────────
  function describeChange(cur, prev) {
    var notes = [];
    Object.keys(cur.targets || {}).forEach(function (id) {
      var a = cur.targets[id];
      var b = (prev.targets || {})[id];
      if (!b) {
        return;
      }
      if (a.ok !== b.ok) {
        notes.push(
          id + (a.ok ? " started answering again" : " stopped answering (" + (a.status || "no response") + ")")
        );
      } else if (a.status !== b.status) {
        notes.push(id + " status " + b.status + " → " + a.status);
      }
    });
    if (cur.ds_sha !== prev.ds_sha) {
      notes.push("Delivery Service deployed " + shortSha(cur.ds_sha));
    }
    if (cur.latest_release !== prev.latest_release) {
      notes.push("release " + (cur.latest_release || "unknown") + " published");
    }
    ["commit_log", "account_keys", "binaries"].forEach(function (k) {
      var a = (cur.trees || {})[k];
      var b = (prev.trees || {})[k];
      if (a != null && b != null && a !== b) {
        notes.push(k.replace("_", " ") + " tree " + b + " → " + a);
      }
    });
    var pa = (cur.publisher || {}).conclusion;
    var pb = (prev.publisher || {}).conclusion;
    if (pa !== pb) {
      notes.push("transparency publisher " + pb + " → " + pa);
    }
    return notes;
  }

  function renderHistory(data) {
    var el = document.getElementById("status-history");
    if (!el) {
      return;
    }
    var samples = (data.samples || []).slice();
    if (!samples.length) {
      el.innerHTML = "";
      return;
    }
    var rows = [];
    for (var i = samples.length - 1; i >= 0 && rows.length < MAX_ROWS; i--) {
      var cur = samples[i];
      var prev = i > 0 ? samples[i - 1] : null;
      var what;
      if (!prev) {
        what = "First observation recorded.";
      } else {
        var notes = describeChange(cur, prev);
        what = notes.length
          ? notes.join("; ")
          : "No change since the previous observation (heartbeat).";
      }
      rows.push(
        '<tr><td class="doc-td-dim">' +
          esc(fmtTime(cur.at)) +
          "</td><td>" +
          badge(cur.reason === "change" ? "info" : "pass", cur.reason || "sample") +
          '</td><td class="doc-td-dim">' +
          esc(what) +
          "</td></tr>"
      );
    }
    el.innerHTML =
      "<caption>The " +
      (rows.length === 1 ? "only recorded observation so far" : rows.length + " most recent recorded observations") +
      ". Every sample ever recorded is in the git history of " +
      "<code>website/status-history.json</code>.</caption>" +
      "<thead><tr><th>Observed (UTC)</th><th>Why recorded</th><th>What changed</th></tr></thead><tbody>" +
      rows.join("") +
      "</tbody>";
  }

  // ── incidents ───────────────────────────────────────────────────────────────
  function severityBadge(sev) {
    if (sev === "sev1") {
      return badge("fail", "sev1 · loss or broken guarantee");
    }
    if (sev === "sev2") {
      return badge("fail", "sev2 · could not send or receive");
    }
    if (sev === "sev3") {
      return badge("warn", "sev3 · degraded");
    }
    return badge("info", "sev4 · no user-visible effect");
  }

  function renderIncidents(record) {
    var el = document.getElementById("status-incidents");
    if (!el) {
      return;
    }
    var incidents = (record.incidents || []).slice().sort(function (a, b) {
      return new Date(b.started_at) - new Date(a.started_at);
    });

    if (!incidents.length) {
      el.innerHTML =
        "<p><strong>No incidents have been recorded since this record opened on " +
        esc(record.record_started_at) +
        ".</strong> Read that literally: it means nothing has been written down " +
        "since that date, not that nothing has ever gone wrong. Pollis kept no " +
        "incident record before then, and this page will not invent a clean " +
        "history it cannot evidence.</p>";
      return;
    }

    el.innerHTML = incidents
      .map(function (inc) {
        var open = inc.status !== "resolved";
        var when =
          fmtTime(inc.started_at) +
          (inc.resolved_at ? " → " + fmtTime(inc.resolved_at) : " → ongoing");
        return (
          '<div class="doc-card"><p>' +
          severityBadge(inc.severity) +
          " " +
          (open ? badge("warn", inc.status) : badge("pass", "resolved")) +
          (inc.correctness_impact
            ? " " + badge("fail", "message-delivery impact")
            : "") +
          "</p><p><strong>" +
          esc(inc.title) +
          '</strong><br /><span class="doc-td-dim">' +
          esc(when) +
          " · " +
          esc((inc.components || []).join(", ")) +
          "</span></p><p>" +
          esc(inc.user_impact) +
          "</p><p>" +
          esc(inc.summary) +
          "</p>" +
          (inc.postmortem
            ? '<p><a href="https://github.com/actuallydan/pollis/blob/main/' +
              esc(inc.postmortem) +
              '" target="_blank" rel="noopener noreferrer">Postmortem</a></p>'
            : "") +
          "</div>"
        );
      })
      .join("");
  }

  function loadFailure(id, message, fallbackHtml) {
    var el = document.getElementById(id);
    if (el) {
      el.innerHTML =
        '<p class="doc-loading">Could not load ' + esc(message) + ". " + fallbackHtml + "</p>";
    }
  }

  fetch(HISTORY_URL, { cache: "no-cache" })
    .then(function (r) {
      if (!r.ok) {
        throw new Error("HTTP " + r.status);
      }
      return r.json();
    })
    .then(function (data) {
      renderCurrent(data);
      renderPublisher(data);
      renderHistory(data);
    })
    .catch(function (err) {
      loadFailure(
        "status-current",
        "the observation record (" + err.message + ")",
        'The same endpoints are open to you directly &mdash; the commands are below, and the raw record is <a href="https://github.com/actuallydan/pollis/blob/main/website/status-history.json" target="_blank" rel="noopener noreferrer">in the repository</a>.'
      );
    });

  fetch(INCIDENTS_URL, { cache: "no-cache" })
    .then(function (r) {
      if (!r.ok) {
        throw new Error("HTTP " + r.status);
      }
      return r.json();
    })
    .then(renderIncidents)
    .catch(function (err) {
      loadFailure(
        "status-incidents",
        "the incident record (" + err.message + ")",
        'It is committed at <a href="https://github.com/actuallydan/pollis/blob/main/website/incidents.json" target="_blank" rel="noopener noreferrer">website/incidents.json</a>.'
      );
    });
})();
