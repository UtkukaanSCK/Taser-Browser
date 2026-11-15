"use strict";
// The overlay webview: address-bar suggestions and the history list. Rust
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

  /** "none", "suggest" or "history". */
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

  function clear() {
    mode = "none";
    hideConfirm();
    suggestList.replaceChildren();
    list.replaceChildren();
    suggestList.hidden = true;
    panel.hidden = true;
  }

  function cleared(ok) {
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
  });
  // No browser menu or text drags in the overlay.
  document.addEventListener("mousedown", (e) => {
    if (e.button === 1) e.preventDefault();
  });

  window.xbo = Object.freeze({ suggest, select, history, clear, cleared });
  send("ov-ready");
})();
