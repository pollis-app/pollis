// Key Transparency explorer — calls the backend verifier and renders its
// structured GroupReport. There is NO in-browser verification here: the browser
// only fetches a server-computed verdict and visualizes it.

// ── Configuration ──────────────────────────────────────────────────────────
// Base URL of the backend verifier (the `serve` dev server's HTTP endpoint).
// Defaults to the local dev server; in production this points at the deployed
// verifier (e.g. https://transparency.pollis.com).
const BACKEND_BASE = "http://127.0.0.1:8787";

// ── DOM helpers ─────────────────────────────────────────────────────────────
const form = document.getElementById("kt-form");
const input = document.getElementById("kt-group");
const submit = document.getElementById("kt-submit");
const result = document.getElementById("kt-result");

function show(html) {
  result.innerHTML = html;
  result.classList.add("is-visible");
}

// Escape text for safe insertion into HTML (all server values are untrusted).
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

// ── Rendering ─────────────────────────────────────────────────────────────--
function renderLoading(id) {
  show('<span class="kt-badge kt-badge--info">Verifying ' + esc(id) + "…</span>");
}

function renderError(message) {
  show(
    '<span class="kt-badge kt-badge--fail">Error</span>' +
      '<p class="kt-note">Could not reach the verifier at <code>' +
      esc(BACKEND_BASE) +
      "</code>. " +
      esc(message) +
      "</p>" +
      '<p class="kt-note">If you are running locally, start the dev server with ' +
      "<code>serve serve --dir &lt;generated tree&gt;</code> and make sure " +
      "<code>BACKEND_BASE</code> in <code>transparency.js</code> points at it.</p>"
  );
}

function renderReport(report) {
  if (!report.found) {
    show(
      '<span class="kt-badge kt-badge--info">Not found</span>' +
        '<p class="kt-note">No commits were found for <code>' +
        esc(report.group_id) +
        "</code> in the transparency log. Double-check the conversation id.</p>"
    );
    return;
  }

  const pass = report.chain_valid;
  let html = "";

  html +=
    '<span class="kt-badge ' +
    (pass ? "kt-badge--pass" : "kt-badge--fail") +
    '">' +
    (pass ? "✓ Chain valid" : "✗ Chain INVALID") +
    "</span>";

  html +=
    '<div class="kt-meta">' +
    "<div>group: " +
    esc(report.group_id) +
    "</div>" +
    "<div>signed tree size: " +
    esc(report.sth_tree_size) +
    "</div>" +
    "<div>root: " +
    esc(report.root_hex) +
    "</div>" +
    "</div>";

  // Violations, if any.
  if (report.violations && report.violations.length > 0) {
    html += '<div class="kt-violations"><h3>Violations</h3><ul>';
    report.violations.forEach(function (v) {
      html += "<li>" + esc(v) + "</li>";
    });
    html += "</ul></div>";
  }

  // Commit timeline.
  html += '<ul class="kt-timeline">';
  report.commits.forEach(function (c) {
    const included = c.included;
    html +=
      '<li class="kt-commit' +
      (included ? "" : " kt-commit--missing") +
      '">' +
      '<div class="kt-commit-head">' +
      '<span class="kt-epoch">epoch ' +
      esc(c.epoch) +
      "</span>" +
      '<span class="kt-inc ' +
      (included ? "kt-inc--ok" : "kt-inc--no") +
      '">' +
      (included ? "included ✓" : "NOT INCLUDED ✗") +
      "</span>" +
      "</div>" +
      '<div class="kt-commit-detail">seq ' +
      esc(c.seq) +
      " · sender " +
      esc(shortHash(c.sender_id)) +
      " · commit " +
      esc(shortHash(c.commit_sha256)) +
      "</div>" +
      "</li>";
  });
  html += "</ul>";

  if (pass) {
    html +=
      '<p class="kt-note">This conversation’s commit history is append-only and ' +
      "fork-free, and every commit is provably included in the signed log.</p>";
  }

  show(html);
}

// ── Submit ──────────────────────────────────────────────────────────────────
form.addEventListener("submit", function (e) {
  e.preventDefault();
  const id = input.value.trim();
  if (!id) {
    return;
  }

  submit.disabled = true;
  renderLoading(id);

  fetch(BACKEND_BASE + "/verify/group/" + encodeURIComponent(id))
    .then(function (resp) {
      if (!resp.ok) {
        // The endpoint returns JSON errors with a 4xx/5xx status (e.g. the
        // underlying artifacts could not be fetched).
        return resp.json().then(
          function (body) {
            throw new Error(body && body.error ? body.error : "HTTP " + resp.status);
          },
          function () {
            throw new Error("HTTP " + resp.status);
          }
        );
      }
      return resp.json();
    })
    .then(function (report) {
      renderReport(report);
    })
    .catch(function (err) {
      renderError(err && err.message ? err.message : "Network error.");
    })
    .finally(function () {
      submit.disabled = false;
    });
});
