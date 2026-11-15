"use strict";
// Bookmarks, both halves (Rust decides everything; see bookmarks.rs):
// - the UI page: the star in the address box (xbBm.star) and its position;
// - the overlay page: the editor and the list (xbBm.editor, xbBm.list).
// The same file is loaded by both pages and does only what its page has
// elements for. Addresses and titles are untrusted: textContent / value only.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);
  const str = (v) => (typeof v === "string" ? v : "");
  const num = (v) => (Number.isSafeInteger(v) && v >= 0 ? v : 0);

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text) e.textContent = text;
    return e;
  }

  // ------------------------------------------------------------ UI page

  const starBtn = $("bm-star");
  let anchor = "";

  /** `can`: this page can be bookmarked; `on`: it is. */
  function star(can, on) {
    if (!starBtn) return;
    starBtn.disabled = !can;
    starBtn.setAttribute("aria-pressed", String(!!(can && on)));
    starBtn.title = can && on ? "Yer imini düzenle (Ctrl+D)" : "Bu sayfayı yer imlerine ekle (Ctrl+D)";
  }

  /** The star's left edge and width, as Rust places the editor under it. */
  function edge() {
    const r = starBtn.getBoundingClientRect();
    return `${Math.max(0, Math.round(r.left))}:${Math.max(0, Math.round(r.width))}`;
  }

  if (starBtn) {
    starBtn.addEventListener("click", () => send("bm-star:" + edge()));
    const report = () => {
      const now = edge();
      if (now === anchor) return;
      anchor = now;
      send("bm-anchor:" + now);
    };
    const box = $("address-box");
    if (box && window.ResizeObserver) new ResizeObserver(report).observe(box);
    window.addEventListener("resize", report);
    report();
  }

  // ------------------------------------------------------- overlay page

  const edit = $("bm-edit");
  const panel = $("bm-list");
  /** "none", "editor" or "list". */
  let active = "none";

  /** A star in front of a suggestion row for a bookmarked address. */
  function mark(row, it) {
    if (!it || it.b !== true) return;
    const s = el("span", "shrink-0 text-accent", "★");
    s.setAttribute("role", "img");
    s.setAttribute("aria-label", "Yer imi");
    row.prepend(s);
  }

  function hide() {
    active = "none";
    if (edit) edit.hidden = true;
    if (panel) panel.hidden = true;
  }

  if (edit && panel) {
    const title = $("bm-title");
    const search = $("bm-search");
    const rows = $("bm-rows");
    const empty = $("bm-empty");
    const more = $("bm-more");
    const remove = $("bm-remove");
    const done = $("bm-done");
    /** The list's generation (Rust ignores picks naming another one). */
    let gen = 0;

    const save = () => send("bm-save:" + title.value);

    function editor(data) {
      hide();
      active = "editor";
      edit.hidden = false;
      $("bm-heading").textContent = data && data.added === true ? "Yer imi eklendi" : "Yer imini düzenle";
      const url = str(data && data.url);
      const p = $("bm-url");
      p.textContent = url;
      p.title = url;
      title.value = str(data && data.title);
      if (data && data.remove === true) {
        remove.focus();
      } else {
        title.focus();
        title.select();
      }
    }

    done.addEventListener("click", save);
    remove.addEventListener("click", () => send("bm-remove"));
    title.addEventListener("keydown", (e) => {
      if (e.key !== "Enter" || e.isComposing) return;
      e.preventDefault();
      save();
    });

    const open = (i, tab) => send(`bm-open:${gen}:${i}:${tab ? "t" : "s"}`);
    const openers = () => [...rows.querySelectorAll("button[data-open]")];

    function row(it, i) {
      const li = el("li", "group flex items-center gap-1 pr-2 hover:bg-hover");
      const url = str(it && it.u);
      const name = str(it && it.t);
      const btn = el("button", "flex min-w-0 flex-1 flex-col gap-1 py-1.5 pl-3 text-left outline-none focus-visible:bg-hover");
      btn.type = "button";
      btn.setAttribute("data-open", "");
      btn.title = url;
      btn.append(el("span", "truncate", name || url));
      if (name) btn.append(el("span", "truncate text-muted text-[12px]", url));
      btn.addEventListener("click", (e) => open(i, e.ctrlKey));
      // Middle click: a new tab.
      btn.addEventListener("auxclick", (e) => {
        if (e.button !== 1) return;
        e.preventDefault();
        open(i, true);
      });
      // One click arms the delete, the second one deletes (as in the history).
      const del = el("button", "h-6 shrink-0 rounded px-1.5 text-muted hover:text-danger data-[armed]:text-danger", "×");
      del.type = "button";
      del.title = "Bu yer imini sil";
      del.setAttribute("aria-label", "Yer imini sil: " + (name || url));
      del.addEventListener("click", () => {
        if (del.hasAttribute("data-armed")) {
          send(`bm-delete:${gen}:${i}`);
        } else {
          del.setAttribute("data-armed", "");
          del.textContent = "Sil?";
        }
      });
      del.addEventListener("blur", () => {
        del.removeAttribute("data-armed");
        del.textContent = "×";
      });
      li.append(btn, del);
      return li;
    }

    /** The list; `keep`: an update of the open list (keep search, focus). */
    function list(data, keep) {
      hide();
      active = "list";
      panel.hidden = false;
      gen = num(data && data.serial);
      const items = Array.isArray(data && data.items) ? data.items : [];
      const total = num(data && data.total);
      const filter = str(data && data.filter);
      if (!keep) search.value = filter;
      rows.replaceChildren(...items.map(row));
      rows.hidden = items.length === 0;
      empty.hidden = items.length > 0;
      empty.textContent = filter ? "Eşleşen yer imi yok" : "Yer imi yok";
      more.hidden = total <= items.length;
      more.textContent = `${total} yer iminden en yeni ${items.length} tanesi gösteriliyor. Aramayı daraltın.`;
      const a = document.activeElement;
      if (!keep || !a || a === document.body) search.focus();
    }

    search.addEventListener("input", () => send("bm-filter:" + search.value));
    search.addEventListener("keydown", (e) => {
      if (e.isComposing) return;
      if (e.key === "Enter") {
        e.preventDefault();
        if (openers().length) open(0, e.ctrlKey);
      } else if (e.key === "ArrowDown") {
        const first = openers()[0];
        if (first) {
          e.preventDefault();
          first.focus();
        }
      }
    });
    rows.addEventListener("keydown", (e) => {
      const t = e.target;
      if (e.isComposing || !(t instanceof HTMLButtonElement) || !t.hasAttribute("data-open")) return;
      const all = openers();
      const at = all.indexOf(t);
      if (e.key === "ArrowDown" && at + 1 < all.length) {
        e.preventDefault();
        all[at + 1].focus();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        (at > 0 ? all[at - 1] : search).focus();
      } else if (e.key === "Enter" && e.ctrlKey) {
        e.preventDefault();
        open(at, true);
      }
    });

    // The editor and the list close once focus goes elsewhere.
    window.addEventListener("blur", () => {
      if (active !== "none") send("ov-close");
    });

    window.xbBm = Object.freeze({ star, editor, list, hide, mark });
  } else {
    window.xbBm = Object.freeze({ star, editor() {}, list() {}, hide, mark });
  }
})();
