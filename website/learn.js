/*
 * learn.js — interactive widgets for the /learn section.
 *
 * Topic 7 (M0 pilot): a live Merkle tree of 8 editable entries. Edit any entry
 * and every hash on the path up to the root recomputes with real SHA-256 via
 * crypto.subtle; the changed path lights up. No dependencies, no network — all
 * hashing is local. Progressive enhancement: the widget markup ships `hidden`
 * and the text/video teach the concept without JS, so we only reveal it once we
 * know we can actually run.
 *
 * Reusable by Topic 8 (inclusion/consistency proofs) and Topic 12.
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
      node.el.textContent = level === 0 ? short(hex) : short(hex);
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
      });
    }

    drawEdges();
    if (window.ResizeObserver) {
      new ResizeObserver(drawEdges).observe(canvas);
    } else {
      window.addEventListener("resize", drawEdges);
    }
    recompute(true);
    widget.hidden = false;
  }

  function init() {
    var widget = document.querySelector("[data-merkle-widget]");
    if (!widget) { return; }
    if (!subtle) { return; }
    build(widget);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
