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
// The one public key everything on this page trusts. This constant is the only
// thing the browser checks: it string-compares the served key to it.
//
// The 2624-hex-char ML-DSA-44 key minted for the #732 rotation. MUST stay
// byte-identical to `PINNED_LOG_PUBLIC_KEY` in
// pollis-core/src/commands/transparency.rs — the two pins are the same claim
// made to two audiences, and a disagreement between them is indistinguishable
// to a visitor from a compromised log. Change them in the same commit, always.
//
// If this is ever set back to `null`, the page reports "cannot check" rather
// than raising an alarm: an absent pin must WITHHOLD trust, never manufacture a
// false one against a served key that is legitimately new.
const PINNED_KEY =
  "56ab128f3f10107382802e69d3de8659d0127c711feb9c849f5b213c6f2d0af3b5fe41f581b202b385906fc42e4421747e84939054d160c551536131e41508a82b1f3ff0a07bcc4cee5e2eae8e85155d5c9e0dbc6e7683811649fb9e3b1f18c7ed070dbf61f2a058915b33f8ad3edcd135dd18770053e5ac971b13d17d95e16e98f47a852d600c47cbc0349354af2898803cfec7112660076d20027cb67870e18fb25ee327a36743fa812ccf93ba0769ddbd3d42ab40849ac8c98357b64eaf1ffc242abb12fddef4d8cdfa02448b4d99546b448e589657f898a47c6f30ddd88edd3f4456470e0a151e5fd601750c8b0489d3471897cfa78e0d7a00d938dfe876ef243117c972e041fdb00aa7af30d34184153cfd7b1e3b481dc562bbfc82bc20fe8ac4d9845f41de49fc33b6f94494df7088b06c7cb9ae35db86ac0fd293ca403046cec46ca9b12c755670d3d9b14c300b11ec292cd5e37d9f9e5e5d1729222a33bf1e13440f44dbf1b4d4104c612db4e269760868be5ff99f9ed269625fa4f39e21713a14293285e95f8a8e8cecd9db8e6a70c36340280322eab3490270ac640f706a23e81d79111dead641eaf7b926582ed0b0422f9addc0091d731a4fe1b9079be8bd75df23f5f9bf287beab7f67f763e04f0245bf9c705136d04eb8391fb4b4f12bfba44ae49bb6f32ddb0d539e59cd0159120b2fb1718f57e12a846638dbe0b650bfcd5a6cc74cd315b49136ea4e13d431a7f3a4c38fc783a82ca2b4c44a2f379c8aa9704d4639de3f94466662c97fbbd834db97a90405c382b5039803f4e4ed5c6b57487c8d23ad9e4d319df3466c49ef1e1cef526ddad1db5fa14f3b067b40580e068582dc428e21dbdc3df848e8e00fe1181f8e0d1409ab9a8757aef008b67191f4368f37cbd587ff65acdf07adbb989d09cc3318e346ca71c029557f2c523c204defab472b3dcb09bfbb95d5d1665a360a00faeb09b660f13fdc00f7b53fbfeaa58f87a208ad4551bcbe4307bf4d8451e027f4cc33cd55700016795c3164b1bc90d9dd1737b49d2e9e4b190128d2e62a44a80c1375c616aa2871ae7ad4a914102551380a8f8edb68c2df02bdf52607a7432ea7026f6a1efcdb37ecc11ecf1623ec6979e5d65c2812a997121010cd5fd9a98b9ed34edc17b667bfd37ef2be6dfe67fbdde03fa95bb80d0e1c7336263042ef44c4f9d28f1bf959bdc24c09cf8269378705022ff476fce91dbba6c8ffec00b27572eaa4835b59948d7a625ccc84ff4ac062176f4972f5131a961b17c7ff0010d2f2f3f8c12b7bf05fb9771d64a24fdab058f4bf3a155ade6a496b9a09d43a7673b5d8fb6519e01bf911ca78cc23f95943f63db72883d522fe24d4b7c7a26c7fd43b4f6f7496acf9ea2cab2e3cd6fc274964b576084c820bae79dbaa331d11751ec718660cd8e7847b7bacf31180803f681fb349b96338c98c791f74bc95e0d37b2810632159bc3175fed2e16038d45d35e4628250e8c9fb66c5bb2238f6456901f657e9655d3d5a09ff4952a0b9eb9c614f78c27626a136ef281f7099f68e898628530ef690851c179ef6a02448d498e49b2c362c839832100f4a9bf4abf17d496c71bfb5263da345d952b275f04707b31b9f6575da6dd2be799b90cc615f52ec32b4833a7e619d7f34f91f16edc38bc0a869c7211473f3ab90255446e0b7efbb2b97e8111d43b039ec0469b020f38925aad61e229836c96fad5bf3c3cad8f2c1c8b56cd819e8972d108dbfa8cd518177feaa7f4e0b547584a9a5d39ad4f1e8010cfead998ec18991cb89031a11c03cbd1ee7e0a1436da10ef154db13d4850c687c0a668215c9c8b7b1c";

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

// Normalise a release version to exactly one leading "v". The two feeds
// disagree: releases/latest.json publishes "v1.5.2" while releases/cli/latest.json
// publishes "1.1.4", so prefixing blindly rendered "vv1.5.2" — and, worse, asked
// the binaries log for a tag `vv1.5.2`, which can never exist, so Release proofs
// always reported the newest desktop release as missing from the log.
function vTag(version) {
  const v = String(version || "").replace(/^v+/, "");
  return v ? "v" + v : "";
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
    (vTag(version) ? esc(vTag(version)) : "version unknown") +
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
  const tag = vTag(version);
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
    (vTag(version) ? esc(vTag(version)) : "version unknown") +
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
// Shown in place of the pin while it is absent: the served key is still worth
// displaying, it simply is not being checked against anything.
function renderKeyPending(served) {
  byId("art-key-hex").innerHTML =
    (served ? copyChip(served, served) : "") +
    '<span class="art-badge art-badge--info">' +
    "no pinned key yet — rotating to ML-DSA-44, nothing is being checked" +
    "</span>";
}

function renderKey(served) {
  if (PINNED_KEY === null) {
    renderKeyPending(served);
    return;
  }
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
  if (PINNED_KEY === null) {
    renderKeyPending("");
    return;
  }
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
