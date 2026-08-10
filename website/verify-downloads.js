// Publish the hash of every download, next to the download (#801).
//
// A transparency log nobody consults proves nothing. The proof already existed —
// every released artifact's SHA-256 is in the public binaries tree — but reaching
// it meant knowing the log exists, finding /artifacts, and reading a proof UI. So
// in practice the people the guarantee is FOR never used it.
//
// This puts the published hash where the download button is, with the exact command
// for the platform, so checking is: run one line, compare two strings.
//
// WHAT IT PROVES, precisely: that the bytes you downloaded are the bytes Pollis
// published and logged for this version. It is not a signature check and it does not
// prove the build matches source (that is /artifacts and the reproducible-build
// work). Understating it here is deliberate — the page must not imply more than a
// hash comparison can carry.
//
// NO in-browser verification, matching artifacts.js: the browser fetches published
// values and DISPLAYS them. Every remote value is escaped before it reaches HTML.

const VERIFY_BASE = "https://verify.pollis.com";
const CDN_BASE = "https://cdn.pollis.com";

// Which log leaf corresponds to the file a person actually downloads.
//
// For the signed platforms that is the `signed` layer — the .dmg / .exe as shipped,
// signature included. For Linux the shipped bytes ARE the payload (the Tauri
// signature is detached), so `payload` is the same file. Picking the wrong layer
// here would show a hash that never matches what a user computes, which is worse
// than showing nothing.
const ROWS = [
  { label: "macOS", platform: "darwin", bundle: "dmg", layer: "signed", file: "pollis-latest-macos.dmg", cmd: (f) => `shasum -a 256 ${f}` },
  { label: "Windows", platform: "windows", bundle: "nsis", layer: "signed", file: "pollis-latest-windows.exe", cmd: (f) => `Get-FileHash ${f}` },
  { label: "Linux · AppImage", platform: "linux", bundle: "appimage", layer: "payload", file: "pollis-latest-linux.AppImage", cmd: (f) => `sha256sum ${f}` },
  { label: "Linux · .deb", platform: "linux", bundle: "deb", layer: "payload", file: "pollis-latest-linux.deb", cmd: (f) => `sha256sum ${f}` },
  { label: "Linux · .rpm", platform: "linux", bundle: "rpm", layer: "payload", file: "pollis-latest-linux.rpm", cmd: (f) => `sha256sum ${f}` },
];

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]),
  );
}

async function fetchJSON(url) {
  const res = await fetch(url, { cache: "no-store" });
  if (!res.ok) {
    throw new Error(`${url}: HTTP ${res.status}`);
  }
  return res.json();
}

// Log entries arrive as hex-encoded JSON records; decode the ones we can and skip
// anything unparseable rather than failing the whole render.
function decodeEntries(doc) {
  const raw = Array.isArray(doc) ? doc : doc.entries || [];
  const out = [];
  for (const e of raw) {
    if (!e || typeof e.data !== "string") {
      continue;
    }
    try {
      const bytes = e.data.match(/.{2}/g).map((h) => parseInt(h, 16));
      out.push(JSON.parse(new TextDecoder().decode(new Uint8Array(bytes))));
    } catch {
      // A record we cannot read is not a record we should guess at.
    }
  }
  return out;
}

function render(container, tag, byKey) {
  const rows = ROWS.map((r) => {
    const hash = byKey.get(`${r.platform}|${r.bundle}|${r.layer}`);
    if (!hash) {
      return "";
    }
    return `
      <li class="verify-row">
        <div class="verify-row-top">
          <span class="verify-plat">${esc(r.label)}</span>
          <code class="verify-cmd">${esc(r.cmd(r.file))}</code>
        </div>
        <div class="verify-row-hash">
          <code class="verify-hash">${esc(hash)}</code>
          <button type="button" class="verify-copy" data-hash="${esc(hash)}"
                  aria-label="Copy the SHA-256 for ${esc(r.label)}">Copy</button>
        </div>
      </li>`;
  }).join("");

  if (!rows.trim()) {
    container.innerHTML =
      `<li class="verify-note">Published hashes for ${esc(tag)} are not in the log yet. ` +
      `They appear shortly after each release.</li>`;
    return;
  }
  container.innerHTML = rows;
}

async function init() {
  const list = document.querySelector("[data-verify-list]");
  const tagEl = document.querySelector("[data-verify-tag]");
  if (!list) {
    return;
  }

  try {
    // Key off the version the download links actually serve, NOT the newest tag in
    // the log. Those can differ for a few minutes around a release, and showing a
    // hash for a build nobody can download yet would look like a mismatch — the one
    // impression this feature must never create.
    const latest = await fetchJSON(`${CDN_BASE}/releases/latest.json`);
    const tag = latest.version;
    if (tagEl && tag) {
      tagEl.textContent = tag;
    }

    const entries = decodeEntries(await fetchJSON(`${VERIFY_BASE}/v1/binaries/entries.json`));
    const byKey = new Map();
    for (const r of entries) {
      if (r.release_tag === tag && r.artifact_sha256) {
        byKey.set(`${r.platform}|${r.bundle}|${r.layer}`, r.artifact_sha256);
      }
    }
    render(list, tag, byKey);
  } catch {
    // Degrade to silence, never to a scary state: a failed fetch here says nothing
    // about the integrity of anyone's download, and must not imply that it does.
    list.innerHTML =
      `<li class="verify-note">Published hashes are temporarily unavailable. ` +
      `They can also be read from the <a href="/artifacts">artifacts page</a>.</li>`;
  }

  list.addEventListener("click", async (ev) => {
    const btn = ev.target.closest(".verify-copy");
    if (!btn) {
      return;
    }
    try {
      await navigator.clipboard.writeText(btn.dataset.hash);
      const was = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(() => {
        btn.textContent = was;
      }, 1200);
    } catch {
      // Clipboard denied — the hash is selectable on the page regardless.
    }
  });
}

init();
