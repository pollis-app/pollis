// Open source page (#1832): renders each product's latest summary.json, written
// by scripts/oss-inventory.mjs and published by scripts/publish-oss-evidence.sh.
// Each product renders on its own and degrades to "not published yet" on a
// failed fetch, so one missing file never blanks the page. Every remote value
// goes through esc() before it reaches HTML.

const OSS_BASE = "https://cdn.pollis.com/releases/oss";
const OSS_PRODUCTS = [
  { id: "desktop", name: "Desktop app" },
  { id: "mobile", name: "Mobile app (iOS and Android)" },
  { id: "cli", name: "Terminal client" },
  { id: "ds", name: "Delivery Service (server)" },
];
const TOP_LICENSES = 8;

function esc(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// Only links to our own CDN are rendered as links.
function cdnLink(url, label) {
  if (typeof url !== "string" || !url.startsWith(`${OSS_BASE}/`)) {
    return esc(label);
  }
  return `<a href="${esc(url)}">${esc(label)}</a>`;
}

function renderLicenses(licenses) {
  const top = licenses.slice(0, TOP_LICENSES);
  const rest = licenses.slice(TOP_LICENSES).reduce((n, l) => n + l.count, 0);
  const rows = top.map((l) => `<tr><td><code>${esc(l.license)}</code></td><td>${esc(l.count)}</td></tr>`);
  if (rest > 0) {
    rows.push(`<tr><td class="doc-td-dim">${esc(licenses.length - TOP_LICENSES)} other licence expressions</td><td>${esc(rest)}</td></tr>`);
  }
  return `<div class="doc-table-wrap"><table class="doc-table">
    <thead><tr><th>Licence</th><th>Packages</th></tr></thead>
    <tbody>${rows.join("")}</tbody></table></div>`;
}

function renderProduct(product, s) {
  const generated = new Date(s.generated);
  const when = Number.isNaN(generated.getTime()) ? "" : ` · ${esc(generated.toISOString().slice(0, 10))}`;
  return `<div class="doc-card">
    <h3 class="doc-sub-title">${esc(product.name)} · <code>${esc(s.version)}</code>${when}</h3>
    <p>${esc(s.components.total)} third-party packages: ${esc(s.components.cargo)} Rust crates and
      ${esc(s.components.npm)} npm packages.
      ${cdnLink(s.sbom, "SBOM (CycloneDX)")}</p>
    ${renderLicenses(s.licenses)}
  </div>`;
}

async function loadProduct(product, slot) {
  try {
    const res = await fetch(`${OSS_BASE}/${product.id}/latest.json`, { cache: "no-cache" });
    if (!res.ok) {
      throw new Error(String(res.status));
    }
    const s = await res.json();
    if (s.schema !== 1 || s.product !== product.id) {
      throw new Error("unexpected summary");
    }
    slot.innerHTML = renderProduct(product, s);
  } catch {
    slot.innerHTML = `<div class="doc-card"><h3 class="doc-sub-title">${esc(product.name)}</h3>
      <p class="doc-td-dim">Not published yet: its next release will publish it.</p></div>`;
  }
}

function initOpenSource() {
  const root = document.getElementById("oss-products");
  if (!root) {
    return;
  }
  root.innerHTML = "";
  for (const product of OSS_PRODUCTS) {
    const slot = document.createElement("div");
    slot.innerHTML = `<p class="doc-loading">Loading ${esc(product.name)}…</p>`;
    root.appendChild(slot);
    loadProduct(product, slot);
  }
}

initOpenSource();
