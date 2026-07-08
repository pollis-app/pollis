// Artifacts dashboard — a live view of every public Pollis output and the
// server-computed transparency proof for each. Like transparency.js, there is
// NO in-browser verification here: the browser fetches server-computed verdicts
// and version pointers and DISPLAYS them. The ONLY thing verified locally is a
// single string compare of the served signing key against the pinned constant.
//
// Every section renders independently and degrades to an "unavailable" state on
// fetch failure, so one dead endpoint never blanks the page. Every remote value
// is escaped through esc() before it is inserted into HTML.

// ── Configuration ──────────────────────────────────────────────────────────
// Server-computed verification API (same trust model / base as transparency.js).
const BACKEND_BASE = "https://verify.pollis.com";
// Static release pointers — the same source of truth index.html uses.
const CDN_BASE = "https://cdn.pollis.com";
// The one Ed25519 public key everything on this page trusts. This constant is
// the only thing the browser checks: it string-compares the served key to it.
const PINNED_KEY =
  "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148";

// ── DOM helpers ─────────────────────────────────────────────────────────────
function byId(id) {
  return document.getElementById(id);
}

// Escape text for safe insertion into HTML (all server/remote values untrusted).
function esc(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function shortHash(s) {
  if (!s || s.length <= 14) {
    return s || "";
  }
  return s.slice(0, 8) + "…" + s.slice(-6);
}

// A copy-to-clipboard chip that shows a shortened value and copies the full one.
// The full value lives in a data attribute (escaped) and is read back — decoded
// — by the delegated click handler below.
function copyChip(fullValue, displayText) {
  return (
    '<button type="button" class="art-copy" data-copy="' +
    esc(fullValue) +
    '" title="Copy full value">' +
    esc(displayText) +
    "</button>"
  );
}

// ── Time formatting (STH timestamps are ms since epoch) ─────────────────────
function fmtUTC(ms) {
  const n = Number(ms);
  if (!isFinite(n) || n <= 0) {
    return "unknown time";
  }
  return new Date(n).toISOString().replace("T", " ").replace(/\.\d+Z$/, "Z");
}

function relativeTime(ms) {
  const n = Number(ms);
  if (!isFinite(n) || n <= 0) {
    return "at an unknown time";
  }
  const diff = Date.now() - n;
  if (diff < 0) {
    return "just now";
  }
  const mins = Math.floor(diff / 60000);
  if (mins < 1) {
    return "moments ago";
  }
  if (mins < 60) {
    return mins + (mins === 1 ? " minute ago" : " minutes ago");
  }
  const hours = Math.floor(mins / 60);
  if (hours < 24) {
    return hours + (hours === 1 ? " hour ago" : " hours ago");
  }
  const days = Math.floor(hours / 24);
  return days + (days === 1 ? " day ago" : " days ago");
}

// ── Fetch helper ────────────────────────────────────────────────────────────
function fetchJSON(url) {
  return fetch(url).then(function (resp) {
    if (!resp.ok) {
      throw new Error("HTTP " + resp.status);
    }
    return resp.json();
  });
}

// ── B1. Desktop app card ────────────────────────────────────────────────────
function renderDesktop(data) {
  const version = data && data.version ? String(data.version) : "";
  const rows = [
    { label: "macOS", key: "macos" },
    { label: "Windows", key: "windows" },
    { label: "Linux .deb", key: "linux_deb" },
    { label: "Linux .rpm", key: "linux_rpm" },
    { label: "Linux .AppImage", key: "linux" },
  ];

  let links = "";
  rows.forEach(function (r) {
    if (data && data[r.key]) {
      links +=
        '<a class="art-link-pill" href="' +
        esc(data[r.key]) +
        '">' +
        esc(r.label) +
        "</a>";
    }
  });
  if (!links) {
    links = '<span class="art-note">No download links published.</span>';
  }

  byId("art-desktop").innerHTML =
    '<div class="art-card-head">' +
    '<span class="art-card-name">Pollis desktop app</span>' +
    '<span class="art-card-ver">' +
    (version ? "v" + esc(version) : "version unknown") +
    "</span>" +
    "</div>" +
    '<p class="art-card-desc">The full Tauri desktop client. Signed installer per platform.</p>' +
    '<div class="art-links">' +
    links +
    "</div>";
}

function renderDesktopUnavailable() {
  byId("art-desktop").innerHTML =
    '<div class="art-card-head">' +
    '<span class="art-card-name">Pollis desktop app</span>' +
    '<span class="art-badge art-badge--info">unavailable</span>' +
    "</div>" +
    '<p class="art-note">Could not reach <code>' +
    esc(CDN_BASE) +
    "/releases/latest.json</code>. Download links are on the " +
    '<a class="art-inline" href="index.html">home page</a>.</p>';
}

// ── B2. Release artifact proofs (binaries transparency) ─────────────────────
function renderReleaseProofs(report, tag) {
  if (!report || !report.found) {
    byId("art-release-proofs").innerHTML =
      '<div class="art-card-head">' +
      '<span class="art-card-name">Release proofs · ' +
      esc(tag) +
      "</span>" +
      '<span class="art-badge art-badge--info">not in log yet</span>' +
      "</div>" +
      '<p class="art-note">No binary-transparency entries were found for <code>' +
      esc(tag) +
      "</code> yet. A release appears here once its hashes are committed to the " +
      "signed binaries log.</p>";
    return;
  }

  const pass = report.chain_valid === true;
  let html =
    '<div class="art-card-head">' +
    '<span class="art-card-name">Release proofs · ' +
    esc(tag) +
    "</span>" +
    '<span class="art-badge ' +
    (pass ? "art-badge--pass" : "art-badge--fail") +
    '">' +
    (pass ? "✓ in transparency log" : "✗ chain INVALID") +
    "</span>" +
    "</div>" +
    '<p class="art-card-desc">Each shipped installer\'s payload hash, and whether it is provably ' +
    "included in the signed binaries log. This proves the <strong>published bytes match</strong> — " +
    "not that every platform is byte-for-byte reproducible from source.</p>" +
    '<div class="art-meta">' +
    "<div>binaries tree size: " +
    esc(report.sth_tree_size) +
    "</div>" +
    "<div>root: " +
    copyChip(report.root_hex || "", shortHash(report.root_hex || "")) +
    "</div>" +
    "</div>";

  // Violations, if any.
  if (report.violations && report.violations.length > 0) {
    html += '<p class="art-note" style="color:#f1707b;">Violations:</p><ul class="art-note">';
    report.violations.forEach(function (v) {
      html += "<li>" + esc(v) + "</li>";
    });
    html += "</ul>";
  }

  const artifacts = report.artifacts || [];
  if (artifacts.length === 0) {
    html += '<p class="art-note">No individual artifacts reported for this tag.</p>';
  } else {
    html +=
      '<div class="art-table-wrap"><table class="art-table"><thead><tr>' +
      "<th>Artifact</th><th>Platform</th><th>Payload SHA-256</th><th>In log</th>" +
      "</tr></thead><tbody>";
    artifacts.forEach(function (a) {
      const included = pass && a.included === true;
      const platform =
        (a.platform ? String(a.platform) : "?") +
        "/" +
        (a.arch ? String(a.arch) : "?");
      html +=
        "<tr>" +
        '<td class="art-mono">' +
        esc(a.artifact_name || a.bundle || "artifact") +
        "</td>" +
        '<td class="art-mono">' +
        esc(platform) +
        "</td>" +
        "<td>" +
        copyChip(a.payload_sha256 || "", shortHash(a.payload_sha256 || "")) +
        "</td>" +
        "<td>" +
        '<span class="art-badge ' +
        (included ? "art-badge--pass" : "art-badge--fail") +
        '">' +
        (included ? "PASS" : "FAIL") +
        "</span>" +
        "</td>" +
        "</tr>";
    });
    html += "</tbody></table></div>";
  }

  html +=
    '<p class="art-note" style="margin-top:1rem;">This verdict is server-computed. Re-run it trustlessly with ' +
    "<code>pollis-verify release " +
    esc(tag) +
    " --base " +
    esc(BACKEND_BASE) +
    "</code>.</p>";

  byId("art-release-proofs").innerHTML = html;
}

function renderReleaseProofsUnavailable(tag) {
  byId("art-release-proofs").innerHTML =
    '<div class="art-card-head">' +
    '<span class="art-card-name">Release proofs' +
    (tag ? " · " + esc(tag) : "") +
    "</span>" +
    '<span class="art-badge art-badge--info">unavailable</span>' +
    "</div>" +
    '<p class="art-note">Could not reach the verifier at <code>' +
    esc(BACKEND_BASE) +
    "</code> for this release's proofs. Try again later, or verify directly with " +
    "<code>pollis-verify release &lt;tag&gt; --base " +
    esc(BACKEND_BASE) +
    "</code>.</p>";
}

function loadReleaseProofs(version) {
  const tag = "v" + version;
  fetchJSON(BACKEND_BASE + "/verify/release/" + encodeURIComponent(tag))
    .then(function (report) {
      renderReleaseProofs(report, tag);
    })
    .catch(function () {
      renderReleaseProofsUnavailable(tag);
    });
}

// ── B3. CLI card ────────────────────────────────────────────────────────────
function renderCLI(data) {
  const version = data && data.version ? String(data.version) : "";
  const rows = [
    { label: "Linux", key: "linux" },
    { label: "macOS", key: "macos" },
    { label: "Windows", key: "windows" },
  ];
  let links = "";
  rows.forEach(function (r) {
    if (data && data[r.key]) {
      links +=
        '<a class="art-link-pill" href="' +
        esc(data[r.key]) +
        '">' +
        esc(r.label) +
        "</a>";
    }
  });
  if (!links) {
    links = '<span class="art-note">No download links published.</span>';
  }

  byId("art-cli").innerHTML =
    '<div class="art-card-head">' +
    '<span class="art-card-name">Pollis CLI (terminal client)</span>' +
    '<span class="art-card-ver">' +
    (version ? "v" + esc(version) : "version unknown") +
    "</span>" +
    "</div>" +
    '<p class="art-card-desc">The self-contained <code>pollis</code> terminal client — same MLS ' +
    "end-to-end encryption, one binary.</p>" +
    '<div class="art-links">' +
    links +
    "</div>";
}

function renderCLIUnavailable() {
  byId("art-cli").innerHTML =
    '<div class="art-card-head">' +
    '<span class="art-card-name">Pollis CLI (terminal client)</span>' +
    '<span class="art-badge art-badge--info">unavailable</span>' +
    "</div>" +
    '<p class="art-note">Could not reach <code>' +
    esc(CDN_BASE) +
    "/releases/cli/latest.json</code>.</p>";
}

// ── C. Daily self-audit — the three signed tree heads ───────────────────────
const TREES = [
  {
    id: "commit-log",
    name: "Commit log",
    desc: "conversation history",
    url: BACKEND_BASE + "/v1/sth/latest.json",
  },
  {
    id: "account-keys",
    name: "Account keys",
    desc: "published identity keys",
    url: BACKEND_BASE + "/v1/account-keys/sth/latest.json",
  },
  {
    id: "binaries",
    name: "Binaries",
    desc: "shipped release hashes",
    url: BACKEND_BASE + "/v1/binaries/sth/latest.json",
  },
];

function treeRowLoading(t) {
  return (
    '<div class="art-tree" id="art-tree-' +
    t.id +
    '">' +
    '<div class="art-tree-head">' +
    '<span class="art-tree-name">' +
    esc(t.name) +
    ' <span class="art-note">· ' +
    esc(t.desc) +
    "</span></span>" +
    '<span class="art-loading">loading…</span>' +
    "</div></div>"
  );
}

function renderTreeRow(t, sth) {
  const row = byId("art-tree-" + t.id);
  if (!row) {
    return;
  }
  const signed = sth && sth.signature ? true : false;
  row.innerHTML =
    '<div class="art-tree-head">' +
    '<span class="art-tree-name">' +
    esc(t.name) +
    ' <span class="art-note">· ' +
    esc(t.desc) +
    "</span></span>" +
    '<span class="art-tree-time">last published ' +
    esc(relativeTime(sth.timestamp)) +
    " (" +
    esc(fmtUTC(sth.timestamp)) +
    ")</span>" +
    "</div>" +
    '<div class="art-tree-detail">' +
    "<span>size " +
    esc(sth.tree_size) +
    "</span>" +
    "<span>root " +
    copyChip(sth.root_hash || "", shortHash(sth.root_hash || "")) +
    "</span>" +
    '<span class="art-badge ' +
    (signed ? "art-badge--info" : "art-badge--fail") +
    '">' +
    (signed ? "signed head published" : "no signature") +
    "</span>" +
    "</div>";
}

function renderTreeRowUnavailable(t) {
  const row = byId("art-tree-" + t.id);
  if (!row) {
    return;
  }
  row.innerHTML =
    '<div class="art-tree-head">' +
    '<span class="art-tree-name">' +
    esc(t.name) +
    ' <span class="art-note">· ' +
    esc(t.desc) +
    "</span></span>" +
    '<span class="art-badge art-badge--info">unavailable</span>' +
    "</div>" +
    '<div class="art-tree-detail"><span class="art-note">No signed head could be fetched for this log.</span></div>';
}

function loadTrees() {
  let rows = "";
  TREES.forEach(function (t) {
    rows += treeRowLoading(t);
  });
  byId("art-trees").innerHTML = rows;

  TREES.forEach(function (t) {
    fetchJSON(t.url)
      .then(function (sth) {
        renderTreeRow(t, sth);
      })
      .catch(function () {
        renderTreeRowUnavailable(t);
      });
  });
}

// ── D. Pinned-key cross-check (the ONLY local verification) ─────────────────
function renderKey(served) {
  const match = served === PINNED_KEY;
  byId("art-key-hex").innerHTML =
    copyChip(PINNED_KEY, PINNED_KEY) +
    '<span class="art-badge ' +
    (match ? "art-badge--pass" : "art-badge--fail") +
    '">' +
    (match ? "✓ served key matches" : "✗ served key DIFFERS") +
    "</span>";
}

function renderKeyUnavailable() {
  byId("art-key-hex").innerHTML =
    copyChip(PINNED_KEY, PINNED_KEY) +
    '<span class="art-badge art-badge--info">served key unavailable</span>';
}

function loadKey() {
  fetchJSON(BACKEND_BASE + "/v1/public_key.json")
    .then(function (doc) {
      const served = doc && doc.public_key ? String(doc.public_key).trim() : "";
      renderKey(served);
    })
    .catch(function () {
      renderKeyUnavailable();
    });
}

// ── Copy delegation ─────────────────────────────────────────────────────────
document.addEventListener("click", function (e) {
  const btn = e.target.closest ? e.target.closest(".art-copy") : null;
  if (!btn) {
    return;
  }
  const value = btn.getAttribute("data-copy") || "";
  if (!navigator.clipboard) {
    return;
  }
  navigator.clipboard.writeText(value).then(function () {
    btn.classList.add("art-copy--copied");
    setTimeout(function () {
      btn.classList.remove("art-copy--copied");
    }, 1500);
  });
});

// ── Boot — every section loads independently ────────────────────────────────
fetchJSON(CDN_BASE + "/releases/latest.json")
  .then(function (data) {
    renderDesktop(data);
    if (data && data.version) {
      loadReleaseProofs(String(data.version));
    } else {
      renderReleaseProofsUnavailable("");
    }
  })
  .catch(function () {
    renderDesktopUnavailable();
    renderReleaseProofsUnavailable("");
  });

fetchJSON(CDN_BASE + "/releases/cli/latest.json")
  .then(function (data) {
    renderCLI(data);
  })
  .catch(function () {
    renderCLIUnavailable();
  });

loadTrees();
loadKey();
