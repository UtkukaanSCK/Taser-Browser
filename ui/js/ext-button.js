"use strict";
// The strip's extensions button (#ext-btn). Rust shows it while an enabled
// extension has a popup (xbExtBtn.show, src/ext_ui.rs) and opens the list
// under it, its right edge at the button's; aria-expanded follows the list
// (xbExtBtn.expanded: ext_ui.rs opens it, overlay.rs closes it).
(() => {
  const btn = ui.$("ext-btn");
  btn.addEventListener("click", () => {
    ui.send("ext-actions:" + Math.max(0, Math.round(btn.getBoundingClientRect().right)));
  });
  btn.setAttribute("aria-expanded", "false");
  window.xbExtBtn = Object.freeze({
    show: (on) => {
      btn.hidden = on !== true;
    },
    expanded: (on) => {
      btn.setAttribute("aria-expanded", on === true ? "true" : "false");
    },
  });
})();
