"use strict";
// The overlay webview: address-bar suggestions, the history list and the
// question before another application is started. Rust
// decides what is shown and where (xbo.*); this page draws it and reports
// picks and keys. Addresses and titles are untrusted: textContent only.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const suggestList = $("suggest");
  const panel = $("history");
  const search = $("h-search");
  const list = $("h-list");
  const empty = $("h-empty");
  const more = $("h-more");
  const actions = $("h-actions");
  const confirmRow = $("h-confirm");
  const confirmText = $("h-confirm-text");
  const status = $("h-status");
  const question = $("launch");
  const questionUri = $("l-uri");
  const questionOpen = $("l-open");
  const questionCancel = $("l-cancel");
  const questionFailed = $("l-failed");
  let questionTimer = 0;
  let guardTimer = 0;
  /** The question's wait (Rust's, plus a margin for the message to get
      there), and when the current one ends (performance.now()). */
  let guardMs = 600;
  let guardUntil = 0;
  /** The last press on "Aç" since its last click: "ok", "early" or "". */
  let openPress = "";

  /** "none", "suggest", "history" or "launch". */
  let mode = "none";
  /** The command the confirm row's "Sil" sends. */
  let confirming = null;

  const str = (v) => (typeof v === "string" ? v : "");
  const num = (v) => (Number.isSafeInteger(v) && v >= 0 ? v : 0);

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text) e.textContent = text;
    return e;
  }

  function when(ts) {
    const d = new Date(num(ts) * 1000);
    return Number.isNaN(d.getTime()) ? "" : d.toLocaleString("tr-TR", { dateStyle: "short", timeStyle: "short" });
  }

  function suggest(data) {
    mode = "suggest";
    hideQuestion();
    panel.hidden = true;
    suggestList.hidden = false;
    const gen = num(data && data.serial);
    const items = Array.isArray(data && data.items) ? data.items : [];
    suggestList.replaceChildren(
      ...items.map((it, i) => {
        const row = el("li", "flex h-[30px] items-center gap-2 px-3 hover:bg-hover data-[sel]:bg-hover");
        row.setAttribute("role", "option");
        row.setAttribute("aria-selected", "false");
        const url = str(it && it.u);
        const title = str(it && it.t);
        row.append(el("span", "min-w-0 shrink truncate", url));
        if (title) row.append(el("span", "min-w-0 flex-1 truncate text-muted", "– " + title));
        if (window.xbBm) xbBm.mark(row, it);
        // The address input keeps focus: a pick is reported on press.
        row.addEventListener("mousedown", (e) => {
          if (e.button !== 0) return;
          e.preventDefault();
          send(`ov-open:${gen}:${i}`);
        });
        return row;
      }),
    );
  }

  function select(i) {
    [...suggestList.children].forEach((row, j) => {
      row.toggleAttribute("data-sel", j === i);
      row.setAttribute("aria-selected", String(j === i));
    });
  }

  function hideConfirm() {
    confirming = null;
    confirmRow.hidden = true;
    actions.hidden = false;
  }

  function askConfirm(text, cmd) {
    confirming = cmd;
    confirmText.textContent = text;
    actions.hidden = true;
    confirmRow.hidden = false;
    $("h-no").focus();
  }

  function historyRow(it, gen, i) {
    const row = el("li", "group flex items-center gap-1 pr-2 hover:bg-hover");
    const open = el("button", "flex min-w-0 flex-1 items-center gap-3 py-1.5 pl-3 text-left outline-none focus-visible:bg-hover");
    open.type = "button";
    const url = str(it && it.u);
    const title = str(it && it.t);
    open.title = url;
    open.append(el("span", "w-28 shrink-0 text-muted tabular-nums", when(it && it.ts)));
    const text = el("span", "flex min-w-0 flex-col gap-1");
    text.append(el("span", "truncate", title || url));
    if (title) text.append(el("span", "truncate text-muted text-[12px]", url));
    open.append(text);
    open.addEventListener("click", () => send(`ov-open:${gen}:${i}`));
    // One click arms the delete, the second one deletes.
    const del = el("button", "h-6 shrink-0 rounded px-1.5 text-muted hover:text-danger data-[armed]:text-danger", "×");
    del.type = "button";
    del.title = "Bu adresi geçmişten sil";
    del.setAttribute("aria-label", "Geçmişten sil: " + (title || url));
    del.addEventListener("click", () => {
      if (del.hasAttribute("data-armed")) {
        send(`ov-delete:${gen}:${i}`);
      } else {
        del.setAttribute("data-armed", "");
        del.textContent = "Sil?";
      }
    });
    del.addEventListener("blur", () => {
      del.removeAttribute("data-armed");
      del.textContent = "×";
    });
    row.append(open, del);
    return row;
  }

  /** The history list; `keep`: an update of the open list (keep search, focus). */
  function history(data, keep) {
    mode = "history";
    hideQuestion();
    suggestList.hidden = true;
    panel.hidden = false;
    const gen = num(data && data.serial);
    const items = Array.isArray(data && data.items) ? data.items : [];
    const total = num(data && data.total);
    const filter = str(data && data.filter);
    if (!keep) {
      search.value = filter;
      hideConfirm();
      status.textContent = "";
    }
    list.replaceChildren(...items.map((it, i) => historyRow(it, gen, i)));
    list.hidden = items.length === 0;
    empty.hidden = items.length > 0;
    empty.textContent = filter ? "Eşleşen kayıt yok" : "Geçmiş boş";
    more.hidden = total <= items.length;
    more.textContent = `${total} kayıttan en yeni ${items.length} tanesi gösteriliyor. Aramayı daraltın.`;
    if (!keep) search.focus();
  }

  function hideQuestion() {
    clearTimeout(questionTimer);
    clearTimeout(guardTimer);
    question.hidden = true;
    questionUri.textContent = "";
    questionOpen.setAttribute("aria-disabled", "true");
    guardUntil = 0;
    openPress = "";
    questionOpen.hidden = false;
    questionFailed.hidden = true;
    questionCancel.textContent = "İptal";
  }

  /** "Aç" counts only after a pause of `guardMs` from now. Shown as off,
      but never `disabled`: a disabled button gets no click, and the presses
      it swallowed would never restart the wait. */
  function holdOpen() {
    clearTimeout(guardTimer);
    guardUntil = performance.now() + guardMs;
    questionOpen.setAttribute("aria-disabled", "true");
    guardTimer = setTimeout(() => questionOpen.removeAttribute("aria-disabled"), guardMs);
  }

  /** "Aç" waits for `ms`: a click aimed at the page a moment ago must not
      land on it. Rust measures the same wait itself and restarts it
      (calling this again) when an answer comes too early. */
  function launchGuard(ms) {
    if (mode !== "launch") return;
    guardMs = (num(ms) || 500) + 100;
    holdOpen();
  }

  /** A press inside the question (a mouse button, Enter or Space), like
      Chromium's InputEventActivationProtector: every press starts the wait
      again, and one that comes before the wait is over is told to Rust,
      which starts its own again too. A page that makes the user click one
      spot fast, where "Aç" will appear, never gets it pressed: "Aç" counts
      only after a pause. */
  function pressed(e) {
    if (mode !== "launch" || questionOpen.hidden) return;
    const early = performance.now() < guardUntil;
    openPress = questionOpen.contains(e.target) ? (early ? "early" : "ok") : "";
    holdOpen();
    if (early) send("launch-press");
  }

  /** The application was not started: say so, and leave only "Kapat". */
  function launchFailed() {
    if (mode !== "launch") return;
    clearTimeout(guardTimer);
    questionOpen.setAttribute("aria-disabled", "true");
    questionOpen.hidden = true;
    questionFailed.hidden = false;
    questionCancel.textContent = "Kapat";
    questionCancel.focus();
  }

  /** The question before another application is started. The answer names
      nothing: Rust opens the address it holds, not one this page sends. */
  function launch(data) {
    clear();
    mode = "launch";
    $("l-scheme").textContent = str(data && data.scheme);
    $("l-origin").textContent = str(data && data.origin) || "bilinmeyen sayfa";
    questionUri.textContent = str(data && data.uri);
    question.hidden = false;
    questionUri.scrollTop = 0;
    questionCancel.focus();
    launchGuard(data && data.guard);
    questionTimer = setTimeout(() => send("launch-cancel"), num(data && data.ms) || 60000);
  }

  function clear() {
    mode = "none";
    hideQuestion();
    hideConfirm();
    suggestList.replaceChildren();
    list.replaceChildren();
    suggestList.hidden = true;
    panel.hidden = true;
    // The menu and the settings page (menu.js, settings.js) hide too.
    document.dispatchEvent(new Event("xb-clear"));
  }

  function cleared(ok) {
    document.dispatchEvent(new CustomEvent("xb-cleared", { detail: ok === true }));
    status.textContent = ok
      ? "Geçmiş, çerezler, site verileri ve önbellek temizlendi."
      : "Geçmiş silindi; çerezler ve önbellek temizlenemedi.";
  }

  search.addEventListener("input", () => send("ov-filter:" + search.value));
  $("h-delete-all").addEventListener("click", () => askConfirm("Tüm geçmiş silinsin mi?", "ov-delete-all"));
  $("h-clear").addEventListener("click", () =>
    askConfirm("Geçmiş, çerezler ve site verileri ile önbellek silinsin mi? Oturumlarınız kapanır.", "ov-clear-data"),
  );
  $("h-yes").addEventListener("click", () => {
    const cmd = confirming;
    hideConfirm();
    if (!cmd) return;
    status.textContent = cmd === "ov-clear-data" ? "Temizleniyor…" : "Geçmiş silindi.";
    send(cmd);
    search.focus();
  });
  $("h-no").addEventListener("click", () => {
    hideConfirm();
    search.focus();
  });

  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || e.isComposing) return;
    e.preventDefault();
    if (confirming) {
      hideConfirm();
      search.focus();
    } else {
      send("ov-esc");
    }
  });
  // The list closes once focus goes elsewhere (the strip or the page).
  window.addEventListener("blur", () => {
    if (mode === "history") send("ov-close");
    // The question waits a moment before it takes that for "no": showing it
    // again can take the keyboard away and give it straight back.
    if (mode === "launch") setTimeout(() => mode === "launch" && !document.hasFocus() && send("launch-cancel"), 100);
  });
  // Presses are looked at before anything inside the question gets them.
  document.addEventListener("pointerdown", pressed, true);
  document.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && pressed(e), true);
  // A click counts if the press it came from was not early; one with no
  // press before it (a screen reader's), if the wait is over now.
  questionOpen.addEventListener("click", () => {
    const press = openPress;
    openPress = "";
    if (press === "early" || (!press && performance.now() < guardUntil)) return;
    send("launch-open");
  });
  questionCancel.addEventListener("click", () => send("launch-cancel"));
  // No browser menu or text drags in the overlay.
  document.addEventListener("mousedown", (e) => {
    if (e.button === 1) e.preventDefault();
  });

  window.xbo = Object.freeze({ suggest, select, history, launch, launchGuard, launchFailed, clear, cleared });
  send("ov-ready");
})();
