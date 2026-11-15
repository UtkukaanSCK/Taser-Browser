"use strict";
// Connection indicator at the start of the address box (#security). Rust
// decides the state (src/policy.rs) and pushes it with every render; this
// only draws it. Every state has its own icon or text and an accessible
// name, never colour alone.
(() => {
  const mark = ui.$("security");
  const text = ui.$("security-text");

  // state: [text shown, accessible name and tooltip]
  const STATES = {
    secure: ["", "Bağlantı güvenli (HTTPS)"],
    insecure: ["Güvenli değil", "Bağlantı güvenli değil (HTTP): bu siteye yazdıklarınızı başkaları görebilir"],
    cert: ["Sertifika hatası", "Sertifika hatası: bu sitenin kimliği doğrulanamadı, bağlantı güvenli değil"],
    crashed: ["Çöktü: yeniden yükle", "Sayfa çöktü. Yeniden yükle (Ctrl+R)"],
  };

  // Only the crashed state is a button (reload); the others just inform.
  mark.addEventListener("click", () => {
    if (mark.dataset.state === "crashed") ui.send("reload");
  });

  window.xbSecurity = Object.freeze({
    /** `state`: "" (no mark), "secure", "insecure", "cert" or "crashed". */
    render(state) {
      const known = Object.hasOwn(STATES, state) ? STATES[state] : null;
      mark.hidden = !known;
      if (!known) {
        delete mark.dataset.state;
        return;
      }
      mark.dataset.state = state;
      // A picture with a name, except while it is the reload button.
      if (state === "crashed") mark.removeAttribute("role");
      else mark.setAttribute("role", "img");
      text.textContent = known[0];
      mark.title = known[1];
      mark.setAttribute("aria-label", known[1]);
    },
  });
})();
