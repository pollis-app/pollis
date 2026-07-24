/*
 * learn.js — interactive widgets for the /learn section.
 *
 * One component, mounted once per `[data-merkle-widget]` element: a live Merkle
 * tree of 8 editable entries. Edit any entry and every hash on the path up to
 * the root recomputes with real SHA-256 via crypto.subtle; the changed path
 * lights up. No dependencies, no network — all hashing is local. Progressive
 * enhancement: the widget markup ships `hidden` and the text/video teach the
 * concept without JS, so we only reveal it once we know we can actually run.
 *
 * Topic 7 (#596) mounts it plain. Topic 8 (#597) mounts the same component with
 * `data-proof`, which adds the inclusion-proof walk: pick an entry, get handed
 * only the sibling hashes on its path, climb one step at a time, and check the
 * computed root against the published one. Every sibling is editable — tamper
 * with one and verification fails, which is the whole point.
 */
(function () {
  "use strict";

  var LEAVES = ["id:ariel", "id:boris", "id:chen", "id:diego",
                "id:esra", "id:farah", "id:gita", "id:hana"];

  // crypto.subtle needs a secure context (https or localhost). If it's missing,
  // leave the widget hidden — the prose + animation already carry the lesson.
  var subtle = window.crypto && window.crypto.subtle;

  function toHex(buf) {
    var b = new Uint8Array(buf), out = "";
    for (var i = 0; i < b.length; i++) {
      out += b[i].toString(16).padStart(2, "0");
    }
    return out;
  }

  function sha256Hex(str) {
    return subtle.digest("SHA-256", new TextEncoder().encode(str)).then(toHex);
  }

  function short(hex) {
    return hex.slice(0, 8) + "…";
  }

  // x-position (fraction of width) of each node in a full 8-leaf binary tree.
  function xFrac(level, i) {
    // level 3 = leaf hashes (8), 2 = pairs (4), 1 = quads (2), 0 = root (1)
    var counts = { 3: 8, 2: 4, 1: 2, 0: 1 };
    var n = counts[level];
    return (i + 0.5) / n;
  }

  // Row y (px) from the top of the canvas, per level.
  var ROW_PAD = 22, ROW_GAP = 58;
  function rowY(level) {
    // level 0 (root) at top → level 3 (leaf hashes) at bottom
    return ROW_PAD + (3 - level) * ROW_GAP;
  }
  var CANVAS_H = ROW_PAD * 2 + 3 * ROW_GAP;

  function build(widget) {
    var canvas = widget.querySelector("[data-canvas]");
    var svg = widget.querySelector("[data-edges]");
    var leafRow = widget.querySelector("[data-leaves]");
    var rootOut = widget.querySelector("[data-root]");
    var proofMode = widget.hasAttribute("data-proof");

    canvas.style.height = CANVAS_H + "px";

    // Node chips: leaf hashes (level 3), then internal levels up to the root.
    // nodes[level] = array of { el, hex, prev }
    var nodes = { 0: [], 1: [], 2: [], 3: [] };
    var levels = [
      { level: 3, count: 8 },
      { level: 2, count: 4 },
      { level: 1, count: 2 },
      { level: 0, count: 1 },
    ];
    levels.forEach(function (spec) {
      for (var i = 0; i < spec.count; i++) {
        var el = document.createElement("div");
        el.className = "ln-node" + (spec.level === 0 ? " ln-node--root" : "");
        el.style.left = xFrac(spec.level, i) * 100 + "%";
        el.style.top = rowY(spec.level) + "px";
        el.textContent = "…";
        canvas.appendChild(el);
        nodes[spec.level].push({ el: el, hex: "", prev: null });
      }
    });

    // Editable leaf inputs, aligned under the leaf-hash chips.
    var inputs = [];
    LEAVES.forEach(function (val, i) {
      var inp = document.createElement("input");
      inp.className = "ln-leaf-input";
      inp.type = "text";
      inp.value = val;
      inp.setAttribute("aria-label", "Entry " + (i + 1));
      inp.spellcheck = false;
      inp.autocomplete = "off";
      inp.addEventListener("input", schedule);
      leafRow.appendChild(inp);
      inputs.push(inp);
    });

    function drawEdges() {
      var w = canvas.clientWidth;
      svg.setAttribute("width", w);
      svg.setAttribute("height", CANVAS_H);
      svg.setAttribute("viewBox", "0 0 " + w + " " + CANVAS_H);
      var lines = "";
      function edge(cl, ci, pl, pi) {
        var x1 = xFrac(cl, ci) * w, y1 = rowY(cl) - 10;
        var x2 = xFrac(pl, pi) * w, y2 = rowY(pl) + 10;
        lines += '<line x1="' + x1 + '" y1="' + y1 + '" x2="' + x2 + '" y2="' +
          y2 + '" stroke="#3f3f46" stroke-width="1.5" />';
      }
      for (var i = 0; i < 8; i++) { edge(3, i, 2, i >> 1); }
      for (var k = 0; k < 4; k++) { edge(2, k, 1, k >> 1); }
      for (var m = 0; m < 2; m++) { edge(1, m, 0, 0); }
      svg.innerHTML = lines;
    }

    function paint(level, i, hex, initial) {
      var node = nodes[level][i];
      var changed = !initial && node.prev !== null && node.prev !== hex;
      node.hex = hex;
      node.el.textContent = short(hex);
      node.el.classList.toggle("ln-node--changed", changed && level !== 0);
      node.prev = hex;
    }

    var pending = null;
    function schedule() {
      if (pending) { return; }
      pending = requestAnimationFrame(function () { pending = null; recompute(false); });
    }

    function recompute(initial) {
      var leafHexes = inputs.map(function (inp) { return sha256Hex(inp.value); });
      Promise.all(leafHexes).then(function (leaf) {
        leaf.forEach(function (hx, i) { paint(3, i, hx, initial); });
        return Promise.all([0, 1, 2, 3].map(function (k) {
          return sha256Hex(leaf[2 * k] + leaf[2 * k + 1]);
        }));
      }).then(function (l1) {
        l1.forEach(function (hx, k) { paint(2, k, hx, initial); });
        return Promise.all([0, 1].map(function (m) {
          return sha256Hex(l1[2 * m] + l1[2 * m + 1]);
        }));
      }).then(function (l2) {
        l2.forEach(function (hx, m) { paint(1, m, hx, initial); });
        return sha256Hex(l2[0] + l2[1]);
      }).then(function (root) {
        paint(0, 0, root, initial);
        rootOut.innerHTML = "root: <b>" + root + "</b>";
        if (proofMode) { refreshProof(); }
      });
    }
    // ── Inclusion-proof walk (Topic 8) ────────────────────────────────────
    // The reader is handed ONLY the siblings on the path from their entry up to
    // the root; every other node greys out. The step rows are built once per
    // (entry, step-count) and then updated in place, so a reader can type into a
    // sibling field without losing the caret — tampering is the point of the
    // widget, and `tampered[n]` overrides the supplied hash until Reset.
    var proof = null;
    var proofUi = null;

    function proofSetup() {
      proofUi = {
        pick: widget.querySelector("[data-proof-pick]"),
        run: widget.querySelector("[data-proof-run]"),
        reset: widget.querySelector("[data-proof-reset]"),
        steps: widget.querySelector("[data-proof-steps]"),
        verdict: widget.querySelector("[data-proof-verdict]"),
      };

      LEAVES.forEach(function (val, i) {
        var opt = document.createElement("option");
        opt.value = String(i);
        opt.textContent = val;
        proofUi.pick.appendChild(opt);
      });
      proofUi.pick.value = "2";

      proofUi.run.addEventListener("click", function () {
        if (!proof) {
          startProof(parseInt(proofUi.pick.value, 10));
        } else if (proof.shown < 3) {
          proof.shown += 1;
          renderProof();
        }
      });
      proofUi.reset.addEventListener("click", clearProof);
      proofUi.pick.addEventListener("change", function () {
        if (proof) { startProof(parseInt(proofUi.pick.value, 10)); }
      });
    }

    // The sibling of the node at (level, i) is its pair partner: index i XOR 1.
    function siblingsFor(leafIndex) {
      return [
        { level: 3, i: leafIndex ^ 1 },
        { level: 2, i: (leafIndex >> 1) ^ 1 },
        { level: 1, i: (leafIndex >> 2) ^ 1 },
      ];
    }

    function startProof(leafIndex) {
      proof = { index: leafIndex, shown: 1, tampered: [null, null, null], sig: "" };
      proofUi.steps.innerHTML = "";
      renderProof();
    }

    function clearProof() {
      proof = null;
      [0, 1, 2, 3].forEach(function (lv) {
        nodes[lv].forEach(function (n) {
          n.el.classList.remove("ln-node--path", "ln-node--sib", "ln-node--dim");
        });
      });
      proofUi.steps.innerHTML = "";
      proofUi.verdict.textContent = "";
      proofUi.verdict.className = "ln-proof-verdict";
      proofUi.run.textContent = "Prove this entry";
    }

    // Re-derive the walk after the tree changes underneath it (entry edited).
    function refreshProof() {
      if (proof) { renderProof(); }
    }

    function el(tag, cls, text) {
      var e = document.createElement(tag);
      if (cls) { e.className = cls; }
      if (text !== undefined) { e.textContent = text; }
      return e;
    }

    // Build the step rows for the current (entry, step-count). Kept separate
    // from the value update so typing into a sibling never rebuilds the DOM.
    function buildSteps() {
      var idx = proof.index;
      proofUi.steps.innerHTML = "";
      proof.rows = [];

      var start = el("div", "ln-proof-row ln-proof-row--start");
      start.appendChild(el("span", "ln-proof-lab", "your entry"));
      var startHex = el("code", "ln-proof-hex", "…");
      start.appendChild(startHex);
      start.appendChild(el("span", "ln-proof-lab", "= sha256 of the entry text"));
      proofUi.steps.appendChild(start);

      for (var n = 0; n < proof.shown; n++) {
        // At each level the node's own index parity decides which side it sits
        // on — the concatenation order is not ours to choose.
        var onLeft = ((idx >> n) & 1) === 0;
        var row = el("div", "ln-proof-row");
        row.appendChild(el("span", "ln-proof-step", String(n + 1)));
        row.appendChild(el("span", "ln-proof-lab", "hash("));

        var acc = el("code", "ln-proof-hex", "…");
        var sib = document.createElement("input");
        sib.className = "ln-proof-sib";
        sib.spellcheck = false;
        sib.autocomplete = "off";
        sib.dataset.sib = String(n);
        sib.setAttribute("aria-label", "Sibling hash supplied for step " + (n + 1));
        sib.addEventListener("input", function () {
          proof.tampered[parseInt(this.dataset.sib, 10)] = this.value.trim();
          updateSteps();
        });

        if (onLeft) {
          row.appendChild(acc);
          row.appendChild(el("span", "ln-proof-lab", "+"));
          row.appendChild(sib);
        } else {
          row.appendChild(sib);
          row.appendChild(el("span", "ln-proof-lab", "+"));
          row.appendChild(acc);
        }
        row.appendChild(el("span", "ln-proof-lab", ") →"));
        var out = el("code", "ln-proof-hex", "…");
        row.appendChild(out);
        proofUi.steps.appendChild(row);

        proof.rows.push({ n: n, onLeft: onLeft, acc: acc, sib: sib, out: out });
      }
      proof.startHex = startHex;
    }

    // Recompute the climb and write the results into the existing rows.
    function updateSteps() {
      var idx = proof.index;
      var sibs = siblingsFor(idx);
      var leafHex = nodes[3][idx].hex;
      proof.startHex.textContent = short(leafHex);

      var carry = leafHex;
      var chain = Promise.resolve();
      proof.rows.forEach(function (r) {
        chain = chain.then(function () {
          var live = nodes[sibs[r.n].level][sibs[r.n].i].hex;
          var sibHex = proof.tampered[r.n] !== null ? proof.tampered[r.n] : live;
          // Only refresh the field from the tree when the reader isn't in it.
          if (proof.tampered[r.n] === null && document.activeElement !== r.sib) {
            r.sib.value = live;
          }
          r.sib.classList.toggle("ln-proof-sib--tampered",
                                 proof.tampered[r.n] !== null && sibHex !== live);
          r.acc.textContent = short(carry);
          return sha256Hex(r.onLeft ? carry + sibHex : sibHex + carry)
            .then(function (out) {
              r.out.textContent = short(out);
              carry = out;
            });
        });
      });

      chain.then(function () {
        if (proof.shown < 3) {
          proofUi.run.textContent = "Next step (" + proof.shown + " of 3)";
          proofUi.verdict.textContent = "";
          proofUi.verdict.className = "ln-proof-verdict";
          return;
        }
        proofUi.run.textContent = "Proof complete";
        var published = nodes[0][0].hex;
        var matched = carry === published;
        proofUi.verdict.className = "ln-proof-verdict " +
          (matched ? "ln-proof-verdict--ok" : "ln-proof-verdict--bad");
        proofUi.verdict.innerHTML = matched
          ? "computed root <b>" + short(carry) + "</b> = published root <b>" +
            short(published) + "</b> — this entry <strong>is in the tree</strong>."
          : "computed root <b>" + short(carry) + "</b> &ne; published root <b>" +
            short(published) + "</b> — <strong>verification fails</strong>. " +
            "No wrong sibling can be made to reach the published root.";
      });
    }

    function renderProof() {
      var idx = proof.index;
      var sibs = siblingsFor(idx);
      var path = [
        { level: 3, i: idx },
        { level: 2, i: idx >> 1 },
        { level: 1, i: idx >> 2 },
        { level: 0, i: 0 },
      ];

      [0, 1, 2, 3].forEach(function (lv) {
        nodes[lv].forEach(function (n) {
          n.el.classList.remove("ln-node--path", "ln-node--sib");
          n.el.classList.add("ln-node--dim");
        });
      });
      path.slice(0, proof.shown + 1).forEach(function (p) {
        nodes[p.level][p.i].el.classList.remove("ln-node--dim");
        nodes[p.level][p.i].el.classList.add("ln-node--path");
      });
      sibs.slice(0, proof.shown).forEach(function (s) {
        nodes[s.level][s.i].el.classList.remove("ln-node--dim");
        nodes[s.level][s.i].el.classList.add("ln-node--sib");
      });

      var sig = proof.index + ":" + proof.shown;
      if (sig !== proof.sig) {
        proof.sig = sig;
        buildSteps();
      }
      updateSteps();
    }


    drawEdges();
    if (window.ResizeObserver) {
      new ResizeObserver(drawEdges).observe(canvas);
    } else {
      window.addEventListener("resize", drawEdges);
    }
    if (proofMode) { proofSetup(); }
    recompute(true);
    widget.hidden = false;
  }

  // ── Topic 11: show the live signed tree head next to the page's example ──
  // Progressive enhancement — the block ships `hidden` and only appears if the
  // fetch actually succeeds, so a blocked request or JS-off reader just sees the
  // curl command and the captured output, which teach the same thing.
  var STH_URL = "https://verify.pollis.com/v1/binaries/sth/latest.json";

  function liveSth() {
    var box = document.querySelector("[data-live-sth]");
    if (!box || typeof fetch !== "function") { return; }
    fetch(STH_URL, { mode: "cors" })
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
      .then(function (sth) {
        if (!sth || !sth.root_hash) { return; }
        box.querySelector("[data-live-size]").textContent = String(sth.tree_size);
        box.querySelector("[data-live-root]").textContent = sth.root_hash;
        box.hidden = false;
      })
      .catch(function () { /* stay hidden — the page reads fine without it */ });
  }

  function init() {
    liveSth();
    if (!subtle) { return; }
    Array.prototype.forEach.call(
      document.querySelectorAll("[data-merkle-widget]"), build);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
