// insights_report.js — clipboard controls for the suggestions block (SDD §9).
//
// Plain ES2019, no build step. Inlined into the report via include_str! and
// run under the document's CSP (script-src 'unsafe-inline'). Exactly two
// behaviours: per-item copy, and copy-all-checked.
//
// The per-item payload lives in the item's data-text attribute, which
// carries real newline bytes (0x0a). The original harness wrote the two
// literal characters backslash-n into the attribute, so the clipboard
// received them; this one does not (SDD §9, T-8).

(function () {
  "use strict";

  var FEEDBACK_MS = 1200;

  function writeClipboard(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
    return Promise.reject(new Error("clipboard unavailable"));
  }

  function finish(button, label, ok) {
    button.textContent = ok ? "Copied" : "Copy failed";
    window.setTimeout(function () {
      button.textContent = label;
    }, FEEDBACK_MS);
  }

  function copyPayload(payload, button, label) {
    writeClipboard(payload).then(
      function () {
        finish(button, label, true);
      },
      function () {
        finish(button, label, false);
      }
    );
  }

  document.addEventListener("click", function (event) {
    var target = event.target;
    if (!target || typeof target.matches !== "function") {
      return;
    }

    if (target.matches(".copy-item")) {
      var item = target.closest(".suggestion-item");
      if (item) {
        copyPayload(item.getAttribute("data-text") || "", target, "Copy");
      }
      return;
    }

    if (target.matches(".copy-all-checked")) {
      var parts = [];
      var boxes = document.querySelectorAll(
        ".suggestion-item input[type=checkbox]:checked"
      );
      for (var i = 0; i < boxes.length; i += 1) {
        var row = boxes[i].closest(".suggestion-item");
        if (row) {
          parts.push(row.getAttribute("data-text") || "");
        }
      }
      // Real newlines between items, not the two-character sequence.
      copyPayload(parts.join("\n\n"), target, "Copy all checked");
    }
  });
})();
