"use strict";
// The settings page in the overlay webview. Rust sends the current values
// (xbs.show) and applies every change it is told of (st-* commands); this page
// only draws them. "Clear browsing data" is the history list's own command.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const page = $("settings");
  const engines = [...page.querySelectorAll('input[name="s-engine"]')];
  const startups = [...page.querySelectorAll('input[name="s-startup"]')];
  const trackings = [...page.querySelectorAll('input[name="s-tracking"]')];
  const autofill = $("s-autofill");
  const folder = $("s-folder");
  const hint = $("s-folder-hint");
  const home = $("s-home");
  const status = $("s-status");
  const actions = $("s-actions");
  const confirmRow = $("s-confirm");

  const MESSAGES = {
    "folder-ok": "İndirme klasörü değiştirildi.",
    "folder-reset": "Varsayılan indirme klasörüne dönüldü.",
    "tracking-failed": "İzleyici engelleme düzeyi bu WebView2 sürümünde değiştirilemedi.",
    "folder-bad": "Var olan bir klasörün tam yolunu girin, örneğin C:\\Users\\ad\\Downloads.",
    "home-ok": "Ana sayfa değiştirildi.",
    "home-reset": "Ana sayfa yeniden yeni sekme sayfası.",
    "home-bad": "Bir web adresi girin, örneğin example.com.",
  };

  const str = (v) => (typeof v === "string" ? v : "");
  const visible = () => !page.hidden;

  function hideConfirm() {
    confirmRow.hidden = true;
    actions.hidden = false;
  }

  /** `keep`: an update of the open page (do not move focus or reset the status). */
  function show(data, keep) {
    const d = data && typeof data === "object" ? data : {};
    page.hidden = false;
    for (const r of engines) r.checked = r.value === d.engine;
    for (const r of startups) r.checked = r.value === d.startup;
    for (const r of trackings) r.checked = r.value === d.tracking;
    autofill.checked = d.autofill !== false;
    const code = str(d.status);
    // What is being typed in the folder box stays, unless the change was its own.
    if (!keep || (code.startsWith("folder") && code !== "folder-bad") || document.activeElement !== folder) folder.value = str(d.folder);
    // The home box the same way.
    if (!keep || (code.startsWith("home") && code !== "home-bad") || document.activeElement !== home) home.value = str(d.home);
    const def = str(d.default_folder);
    hint.textContent = d.folder ? "" : def ? "Varsayılan: " + def : "";
    if (!keep) {
      hideConfirm();
      status.textContent = "";
      (engines.find((r) => r.checked) || engines[0]).focus();
    }
    if (code) status.textContent = MESSAGES[code] || "";
  }

  function hide() {
    page.hidden = true;
    hideConfirm();
  }

  for (const r of engines) r.addEventListener("change", () => r.checked && send("st-engine:" + r.value));
  for (const r of startups) r.addEventListener("change", () => r.checked && send("st-startup:" + r.value));
  for (const r of trackings) r.addEventListener("change", () => r.checked && send("st-tracking:" + r.value));
  autofill.addEventListener("change", () => send("st-autofill:" + (autofill.checked ? "1" : "0")));
  const applyFolder = () => send("st-folder:" + folder.value.trim());
  $("s-folder-apply").addEventListener("click", applyFolder);
  $("s-folder-reset").addEventListener("click", () => send("st-folder:"));
  folder.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      applyFolder();
    }
  });
  const applyHome = () => send("st-home:" + home.value.trim());
  $("s-home-apply").addEventListener("click", applyHome);
  $("s-home-reset").addEventListener("click", () => send("st-home:"));
  home.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      applyHome();
    }
  });

  $("s-clear").addEventListener("click", () => {
    actions.hidden = true;
    confirmRow.hidden = false;
    $("s-no").focus();
  });
  $("s-yes").addEventListener("click", () => {
    hideConfirm();
    status.textContent = "Temizleniyor…";
    send("ov-clear-data");
    $("s-clear").focus();
  });
  $("s-no").addEventListener("click", () => {
    hideConfirm();
    $("s-clear").focus();
  });

  // Esc first backs out of the question; only then it closes the page (the
  // overlay's own handler, which hears it after this one).
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape" && visible() && !confirmRow.hidden) {
        e.stopImmediatePropagation();
        e.preventDefault();
        hideConfirm();
        $("s-clear").focus();
      }
    },
    true,
  );

  document.addEventListener("xb-cleared", (e) => {
    if (!visible()) return;
    status.textContent = e.detail
      ? "Geçmiş, çerezler, site verileri ve önbellek temizlendi."
      : "Geçmiş silindi; çerezler ve önbellek temizlenemedi.";
  });
  window.addEventListener("blur", () => {
    if (visible()) send("ov-close");
  });
  document.addEventListener("xb-clear", hide);

  window.xbs = Object.freeze({ show });
})();
