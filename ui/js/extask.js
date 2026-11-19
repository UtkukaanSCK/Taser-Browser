"use strict";
// The Web Store question in the overlay (#ext-ask, src/webstore.rs): add or
// remove an extension. Rust decides what is shown and has the last word on
// every answer; the name and the permissions come from the package, so they
// are written as text only. Like the launch question (overlay.js): the
// answer button counts only after a pause in pressing, a lost focus is "no",
// and it waits a limited time.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const box = $("ext-ask");
  const title = $("x-title");
  const name = $("x-name");
  const meta = $("x-meta");
  const note = $("x-note");
  const perms = $("x-perms");
  const yes = $("x-yes");
  const no = $("x-no");

  /** Shown, and which stage: "fetch", "ask", "adding", "removing", "error". */
  let on = false;
  let stage = "";
  let timer = 0;
  let guardTimer = 0;
  let guardMs = 600;
  let guardUntil = 0;
  /** The last press on the answer button: "ok", "early" or "". */
  let yesPress = "";

  const str = (v) => (typeof v === "string" ? v : "");
  const num = (v) => (Number.isSafeInteger(v) && v >= 0 ? v : 0);

  function hide() {
    on = false;
    stage = "";
    clearTimeout(timer);
    clearTimeout(guardTimer);
    box.hidden = true;
    for (const e of [title, name, meta, note]) e.textContent = "";
    perms.replaceChildren();
    yes.setAttribute("aria-disabled", "true");
    guardUntil = 0;
    yesPress = "";
  }

  /** The answer button counts only after a pause of `guardMs` from now.
      Shown as off, never `disabled` (see overlay.js). */
  function hold() {
    clearTimeout(guardTimer);
    guardUntil = performance.now() + guardMs;
    yes.setAttribute("aria-disabled", "true");
    guardTimer = setTimeout(() => yes.removeAttribute("aria-disabled"), guardMs);
  }

  /** Rust's wait, plus a margin; called again when an answer came early. */
  function guard(ms) {
    if (!on || stage !== "ask") return;
    guardMs = (num(ms) || 500) + 100;
    hold();
  }

  function pressed(e) {
    if (!on || stage !== "ask") return;
    const early = performance.now() < guardUntil;
    yesPress = yes.contains(e.target) ? (early ? "early" : "ok") : "";
    hold();
    if (early) send("ext-press");
  }

  const TEXT = {
    fetch: ["Uzantı hazırlanıyor", "Chrome Web Mağazası'ndan indiriliyor ve doğrulanıyor…", "İptal"],
    adding: ["Uzantı ekleniyor…", "Bu pencereyi kapatsanız da ekleme sürer.", "Kapat"],
    removing: ["Uzantı kaldırılıyor…", "", "Kapat"],
    error: ["İşlem tamamlanamadı", "", "Kapat"],
  };

  function show(d) {
    window.xbo.clear();
    on = true;
    stage = str(d && d.stage);
    const remove = str(d && d.action) === "remove";
    const update = !remove && d && d.update === true;
    const id = str(d && d.id);
    const version = str(d && d.version);
    name.textContent = str(d && d.name) || (stage === "error" ? "" : id);
    meta.textContent = id ? (version ? `Sürüm ${version} · ` : "") + `Kimlik: ${id}` : "";
    perms.replaceChildren();
    perms.hidden = true;
    yes.hidden = stage !== "ask";
    if (stage === "ask") {
      title.textContent = remove ? "Bu uzantı kaldırılsın mı?" : update ? "Bu uzantı güncellensin mi?" : "Bu uzantı eklensin mi?";
      yes.textContent = remove ? "Kaldır" : update ? "Güncelle" : "Ekle";
      no.textContent = "İptal";
      if (remove) {
        note.textContent = "Uzantının ayarları ve kaydettiği veriler de silinir.";
      } else {
        const lines = Array.isArray(d && d.permissions) ? d.permissions.map(str).filter(Boolean) : [];
        // An update lists only what the new version adds.
        note.textContent = update
          ? lines.length ? "Yeni sürüm şu yeni izinleri istiyor:" : "Yeni sürüm daha geniş erişim istiyor."
          : lines.length ? "Bu uzantı şunları yapabilir:" : "Bu uzantı özel bir izin istemiyor.";
        perms.replaceChildren(...lines.map((t) => {
          const li = document.createElement("li");
          li.textContent = t;
          return li;
        }));
        perms.hidden = lines.length === 0;
      }
    } else {
      const t = Object.hasOwn(TEXT, stage) ? TEXT[stage] : TEXT.error;
      title.textContent = t[0];
      note.textContent = stage === "error" ? str(d && d.text) : t[1];
      no.textContent = t[2];
    }
    box.hidden = false;
    perms.scrollTop = 0;
    no.focus();
    if (stage === "ask") {
      guard(d && d.guard);
      timer = setTimeout(() => send("ext-no"), num(d && d.ms) || 60000);
    }
  }

  // Whatever else the overlay shows, or its clearing, hides the question.
  document.addEventListener("xb-clear", hide);
  window.addEventListener("blur", () => {
    if (on) setTimeout(() => on && !document.hasFocus() && send("ext-no"), 100);
  });
  document.addEventListener("pointerdown", pressed, true);
  document.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && pressed(e), true);
  yes.addEventListener("click", () => {
    const press = yesPress;
    yesPress = "";
    if (stage !== "ask" || press === "early" || (!press && performance.now() < guardUntil)) return;
    send("ext-yes");
  });
  no.addEventListener("click", () => send("ext-no"));

  window.xbExt = Object.freeze({ show, guard });
})();
