"use strict";
// Connection indicator at the start of the address box (#security). Rust
// decides the state (src/policy.rs) and pushes it with every render; this
// only draws it. Every state has its own icon or text, never colour alone.
// What the mark means is written into #security-status, which screen readers
// read out when it changes. The mark's icon and text sit in a button that
// opens the site information panel (src/site_info.rs) under it, or closes it;
// a crashed page adds a second button that reloads it.
(() => {
  const mark = ui.$("security");
  const text = ui.$("security-text");
  const reload = ui.$("security-reload");
  const status = ui.$("security-status");
  const info = ui.$("site-info");

  // state: [text shown, sentence for the tooltip and for screen readers]
  const STATES = {
    secure: ["", "Bağlantı güvenli (HTTPS)"],
    insecure: ["Güvenli değil", "Bağlantı güvenli değil (HTTP): bu siteye yazdıklarınızı başkaları görebilir"],
    cert: ["Sertifika hatası", "Sertifika hatası: bu sitenin kimliği doğrulanamadı, bağlantı güvenli değil"],
    crashed: ["Sayfa çöktü", "Sayfa çöktü. Yeniden yüklemek için Ctrl+R"],
  };
  // "cert" once Rust distrusts every https page for the rest of the session
  // (too many certificate errors to remember each site). The new sentence
  // is read out once, when it first appears, like any change of state.
  const DISTRUST = [
    "Sertifikalara güvenilmiyor",
    "Bu oturumda çok fazla sertifika hatası oldu: tarayıcı yeniden başlatılana kadar hiçbir HTTPS sayfası güvenli sayılmaz",
  ];

  reload.addEventListener("click", () => ui.send("reload"));

  // A press on the button while its panel is up: the panel closes as the UI
  // takes the focus (blur), so the click that follows must not open it again.
  let wasOpen = false;
  info.addEventListener("pointerdown", () => {
    wasOpen = info.getAttribute("aria-expanded") === "true";
  });
  info.addEventListener("click", () => {
    const skip = wasOpen;
    wasOpen = false;
    if (skip) return;
    ui.send("site-info:" + Math.max(0, Math.round(info.getBoundingClientRect().left)));
  });

  window.xbSecurity = Object.freeze({
    /** `state`: "" (no mark), "secure", "insecure", "cert" or "crashed".
     *  `distrust`: no https page counts as secure until the next start. */
    render(state, distrust) {
      const known = state === "cert" && distrust === true ? DISTRUST : Object.hasOwn(STATES, state) ? STATES[state] : null;
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
      info.title = known[1] + " · Site bilgileri";
    },
    /** The panel under the mark is up (`on`) or not. */
    expanded(on) {
      info.setAttribute("aria-expanded", on ? "true" : "false");
    },
  });
})();
