"use strict";
// Connection indicator at the start of the address box (#security). Rust
// decides the state (src/policy.rs) and pushes it with every render; this
// only draws it. Every state has its own icon or text, never colour alone.
// The mark is no control and has no role: what it means is written into
// #security-status, which screen readers read out when it changes. Only a
// crashed page adds a control, a real button that reloads it.
(() => {
  const mark = ui.$("security");
  const text = ui.$("security-text");
  const reload = ui.$("security-reload");
  const status = ui.$("security-status");

  // state: [text shown, sentence for the tooltip and for screen readers]
  const STATES = {
    secure: ["", "Bağlantı güvenli (HTTPS)"],
    insecure: ["Güvenli değil", "Bağlantı güvenli değil (HTTP): bu siteye yazdıklarınızı başkaları görebilir"],
    cert: ["Sertifika hatası", "Sertifika hatası: bu sitenin kimliği doğrulanamadı, bağlantı güvenli değil"],
    crashed: ["Sayfa çöktü", "Sayfa çöktü. Yeniden yüklemek için Ctrl+R"],
  };

  reload.addEventListener("click", () => ui.send("reload"));

  window.xbSecurity = Object.freeze({
    /** `state`: "" (no mark), "secure", "insecure", "cert" or "crashed". */
    render(state) {
      const known = Object.hasOwn(STATES, state) ? STATES[state] : null;
      const said = known ? known[1] : "";
      // Written only when it changes: every render comes through here, and
      // the same sentence must not be read out again.
      if (status.textContent !== said) status.textContent = said;
      mark.hidden = !known;
      reload.hidden = state !== "crashed";
      if (!known) {
        delete mark.dataset.state;
        return;
      }
      mark.dataset.state = state;
      text.textContent = known[0];
      mark.title = known[1];
    },
  });
})();
