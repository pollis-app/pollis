// The one copy-to-clipboard button for every page (#1731). Include it with
// <script src="copy.js"></script>; it needs no other setup.
//
// Markup, on any <button>:
//   data-copy="value"          copies this value (escape it like any attribute), or
//   data-copy-from="element-id" copies that element's text (an install command).
//   data-copied-label="Copied" optional: the button's text while it shows the confirmation.
//
// After a successful copy the button carries `data-copied` for COPIED_MS; each
// page styles that state in CSS (a check icon, an accent border). If the
// clipboard is unavailable or denied nothing changes: the value stays on the
// page to select by hand.
(function () {
  var COPIED_MS = 1500;

  function valueOf(btn) {
    if (btn.hasAttribute("data-copy")) {
      return btn.getAttribute("data-copy") || "";
    }
    var source = document.getElementById(btn.getAttribute("data-copy-from"));
    return source ? source.textContent : "";
  }

  function showCopied(btn) {
    var label = btn.getAttribute("data-copied-label");
    if (btn._copyTimer) {
      clearTimeout(btn._copyTimer);
    } else if (label) {
      btn._copyRestore = btn.textContent;
      btn.textContent = label;
    }
    btn.setAttribute("data-copied", "");
    btn._copyTimer = setTimeout(function () {
      btn.removeAttribute("data-copied");
      if (label) {
        btn.textContent = btn._copyRestore;
      }
      btn._copyTimer = null;
    }, COPIED_MS);
  }

  document.addEventListener("click", function (e) {
    var btn = e.target && e.target.closest ? e.target.closest("button[data-copy], button[data-copy-from]") : null;
    if (!btn || !navigator.clipboard) {
      return;
    }
    navigator.clipboard.writeText(valueOf(btn)).then(
      function () {
        showCopied(btn);
      },
      function () {
        // Clipboard denied: the value is selectable on the page regardless.
      }
    );
  });
})();
