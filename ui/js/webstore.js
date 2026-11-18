"use strict";
// The Web Store button in the address box (#ext-store). Rust decides its
// state for the page shown (src/webstore.rs) and pushes it with every
// render; a click only asks Rust, which asks the user in its own question.
(() => {
  const btn = ui.$("ext-store");
  const text = ui.$("ext-store-text");

  // state: [text shown, sentence for the tooltip and for screen readers]
  const STATES = {
    add: ["Ekle", "Bu uzantıyı X-Browser'a ekle"],
    remove: ["Kaldır", "Bu uzantıyı X-Browser'dan kaldır"],
    busy: ["…", "Uzantı işlemi sürüyor"],
  };

  btn.addEventListener("click", () => {
    if (btn.getAttribute("aria-busy") !== "true") ui.send("ext-store");
  });

  window.xbStore = Object.freeze({
    /** `state`: "" (not a store detail page), "add", "remove" or "busy". */
    render(state) {
      const known = Object.hasOwn(STATES, state) ? STATES[state] : null;
      btn.hidden = !known;
      if (!known) return;
      if (text.textContent !== known[0]) text.textContent = known[0];
      btn.title = known[1];
      btn.setAttribute("aria-label", known[1]);
      btn.setAttribute("aria-busy", String(state === "busy"));
    },
  });
})();
