"use strict";
// The Uzantılar page and the strip's extensions button's list, in the
// overlay webview. Rust sends the rows (xbx.show) and does every operation
// (ext-* commands, src/ext_ui.rs); this page only draws them, asks before a
// removal and keeps the keyboard focus where the user was. Everything an
// extension names (its name, its version) is text: never markup.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const page = $("extensions");
  const title = $("x-title");
  const list = $("x-list");
  const empty = $("x-empty");
  const status = $("x-status");
  const menu = $("ext-actions");

  const ID = /^[a-p]{32}$/;
  // Rust sends only small PNGs read from the extension's own folder.
  const ICON = "data:image/png;base64,";
  const STATES = {
    pending: "İşleniyor…",
    error: "İşlem tamamlanamadı.",
    timeout: "Yanıt gelmedi; yeniden deneyin.",
  };
  const NOTES = {
    removed: (n) => `${n} kaldırıldı.`,
    "remove-failed": (n) => `${n} kaldırılamadı.`,
  };

  const str = (v) => (typeof v === "string" ? v : "");
  const visible = () => !page.hidden;
  const menuVisible = () => !menu.hidden;

  // The extension whose removal is being asked about.
  let confirming = null;
  // The control the user last had: { id, ctl }. A redraw puts the focus
  // back there (or as near as is left).
  let want = null;

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function puzzle() {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", "0 0 16 16");
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", "M3 5.5h2.5a1.5 1.5 0 1 1 3 0H11v2.5a1.5 1.5 0 1 1 0 3V13.5H8.5a1.5 1.5 0 1 0-3 0H3v-2.5a1.5 1.5 0 1 0 0-3z");
    svg.append(path);
    return svg;
  }

  /** The extension's icon, or a puzzle piece. Decorative: the name says it. */
  function icon(src, size) {
    const s = str(src);
    if (s.startsWith(ICON)) {
      const img = el("img", `${size} shrink-0 object-contain`);
      img.alt = "";
      img.draggable = false;
      img.src = s;
      return img;
    }
    const ph = el("span", `flex ${size} shrink-0 items-center justify-center rounded text-muted`);
    ph.setAttribute("aria-hidden", "true");
    ph.append(puzzle());
    return ph;
  }

  function button(text, ctl, cls) {
    const b = el("button", cls || "h-7 rounded-md border border-line px-3 hover:bg-hover disabled:opacity-40 disabled:hover:bg-transparent focus-visible:outline-2 focus-visible:outline-accent", text);
    b.type = "button";
    b.dataset.ctl = ctl;
    return b;
  }

  function row(x) {
    const busy = x.state === "pending";
    const li = el("li", "flex flex-col gap-2 border-b border-line px-4 py-3 outline-none last:border-b-0");
    li.dataset.id = x.id;
    li.tabIndex = -1;
    li.setAttribute("aria-label", x.name);
    if (busy) li.setAttribute("aria-busy", "true");

    const top = el("div", "flex items-center gap-3");
    top.append(icon(x.icon, "size-6"));
    const text = el("div", "flex min-w-0 flex-1 flex-col gap-1");
    text.append(el("span", "truncate font-semibold leading-snug", x.name));
    const facts = [];
    if (x.version) facts.push("Sürüm " + x.version);
    if (x.unknown) facts.push("X-Browser dışında kurulmuş");
    const sub = el("span", "text-[12px] text-muted", facts.join(" · "));
    text.append(sub);
    top.append(text);

    const label = el("label", "flex shrink-0 items-center gap-2");
    const sw = document.createElement("input");
    sw.type = "checkbox";
    sw.className = "accent-accent";
    sw.setAttribute("role", "switch");
    sw.setAttribute("aria-label", x.name + " etkin");
    sw.dataset.ctl = "switch";
    sw.checked = x.enabled;
    sw.disabled = busy;
    sw.addEventListener("change", () => send(`ext-enable:${x.id}:${sw.checked ? "1" : "0"}`));
    label.append(sw, el("span", "", x.enabled ? "Açık" : "Kapalı"));
    top.append(label);
    li.append(top);

    const actions = el("div", "flex flex-wrap items-center gap-2");
    const options = button("Seçenekler", "options");
    const canOpen = x.options && x.enabled && x.installed;
    options.disabled = busy || !canOpen;
    if (!x.options) options.title = "Bu uzantının seçenekler sayfası yok";
    else if (!x.enabled) options.title = "Önce uzantıyı açın";
    options.addEventListener("click", () => send("ext-options:" + x.id));
    const remove = button("Kaldır", "remove");
    remove.disabled = busy;
    remove.addEventListener("click", () => {
      confirming = x.id;
      want = { id: x.id, ctl: "no" };
      redraw();
    });
    actions.append(options, remove);
    const state = el("p", "ml-auto leading-snug " + (busy ? "text-muted" : "text-danger"), STATES[x.state] || "");
    state.setAttribute("role", "status");
    actions.append(state);
    li.append(actions);

    if (confirming === x.id && !busy) {
      actions.hidden = true;
      const ask = el("div", "flex flex-wrap items-center gap-2");
      ask.setAttribute("role", "group");
      ask.setAttribute("aria-label", "Kaldırma onayı");
      ask.append(el("span", "mr-auto leading-snug", `${x.name} kaldırılsın mı? Uzantının ayarları da silinir.`));
      const yes = button("Kaldır", "yes", "h-7 rounded-md border border-danger px-3 text-danger hover:bg-hover focus-visible:outline-2 focus-visible:outline-accent");
      yes.addEventListener("click", () => {
        confirming = null;
        want = { id: x.id, ctl: "remove" };
        send("ext-remove:" + x.id);
        redraw();
      });
      const no = button("Vazgeç", "no");
      no.addEventListener("click", () => {
        confirming = null;
        want = { id: x.id, ctl: "remove" };
        redraw();
      });
      ask.append(yes, no);
      li.append(ask);
    }
    return li;
  }

  let items = [];

  function clean(data) {
    const d = data && typeof data === "object" ? data : {};
    const out = [];
    for (const x of Array.isArray(d.items) ? d.items : []) {
      if (!x || !ID.test(str(x.id))) continue;
      out.push({
        id: x.id,
        name: str(x.name) || x.id,
        version: str(x.version),
        enabled: x.enabled === true,
        installed: x.installed === true,
        options: x.options === true,
        unknown: x.unknown === true,
        icon: str(x.icon),
        state: str(x.state),
      });
    }
    return out;
  }

  /** Puts the focus on `want`, or as near to it as the rows allow. */
  function refocus(before) {
    if (!want) return;
    const rows = [...list.children];
    const at = rows.findIndex((r) => r.dataset.id === want.id);
    if (at >= 0) {
      const c = rows[at].querySelector(`[data-ctl="${want.ctl}"]`);
      if (c && !c.disabled && c.offsetParent !== null) c.focus();
      else rows[at].focus();
      return;
    }
    // The row is gone (removed): the one now in its place, else the title.
    const i = Math.min(before, rows.length - 1);
    want = null;
    if (i >= 0) {
      const sw = rows[i].querySelector('[data-ctl="switch"]');
      (sw && !sw.disabled ? sw : rows[i]).focus();
    } else {
      title.focus();
    }
  }

  function redraw() {
    const before = want ? [...list.children].findIndex((r) => r.dataset.id === want.id) : -1;
    const inside = page.contains(document.activeElement) && document.activeElement !== title;
    if (confirming && !items.some((x) => x.id === confirming && x.state !== "pending")) confirming = null;
    list.replaceChildren(...items.map(row));
    list.hidden = items.length === 0;
    empty.hidden = items.length > 0;
    if (inside || (want && before >= 0)) refocus(Math.max(before, 0));
  }

  /** `keep`: an update of the open page (the focus stays where it is). */
  function show(data, keep) {
    items = clean(data);
    const d = data && typeof data === "object" ? data : {};
    menu.hidden = true;
    page.hidden = false;
    if (!keep) {
      confirming = null;
      want = null;
    }
    redraw();
    const note = d.note && typeof d.note === "object" ? d.note : null;
    const say = note && NOTES[str(note.code)];
    status.textContent = say ? say(str(note.name)) : "";
    if (!keep) {
      const first = list.querySelector('[data-ctl="switch"]');
      (first || title).focus();
    }
  }

  function hide() {
    page.hidden = true;
    menu.hidden = true;
    confirming = null;
    want = null;
    items = [];
    list.replaceChildren();
    menu.replaceChildren();
    status.textContent = "";
  }

  page.addEventListener("focusin", (e) => {
    const c = e.target.closest("[data-ctl]");
    const li = e.target.closest("li[data-id]");
    if (c && li) want = { id: li.dataset.id, ctl: c.dataset.ctl };
  });

  // ---------------------------------------------------------------- list
  // The button's list: one row per enabled extension with a popup.

  function actions(rows) {
    page.hidden = true;
    menu.replaceChildren();
    for (const r of Array.isArray(rows) ? rows : []) {
      if (!r || !ID.test(str(r.id))) continue;
      const b = el("button", "mi h-9");
      b.type = "button";
      b.setAttribute("role", "menuitem");
      b.dataset.id = r.id;
      b.append(icon(r.icon, "size-4"), el("span", "truncate", str(r.name) || r.id));
      b.addEventListener("click", () => send("ext-popup:" + r.id));
      menu.append(b);
    }
    menu.hidden = false;
    const first = menu.querySelector("button");
    if (first) first.focus();
  }

  function move(delta) {
    const stops = [...menu.querySelectorAll("button")];
    if (!stops.length) return;
    const at = stops.indexOf(document.activeElement);
    const next = at < 0 ? (delta > 0 ? 0 : stops.length - 1) : (at + delta + stops.length) % stops.length;
    stops[next].focus();
  }

  document.addEventListener("keydown", (e) => {
    if (!menuVisible() || e.isComposing) return;
    const plain = !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;
    const stops = menu.querySelectorAll("button");
    let handled = true;
    if (plain && e.key === "ArrowDown") move(1);
    else if (plain && e.key === "ArrowUp") move(-1);
    else if (plain && e.key === "Home" && stops.length) stops[0].focus();
    else if (plain && e.key === "End" && stops.length) stops[stops.length - 1].focus();
    else if (e.key === "Tab" && !e.ctrlKey && !e.altKey) move(e.shiftKey ? -1 : 1);
    else handled = false;
    if (handled) e.preventDefault();
  });

  // Esc first backs out of the question; only then it closes the page (the
  // overlay's own handler, which hears it after this one).
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape" && visible() && confirming) {
        e.stopImmediatePropagation();
        e.preventDefault();
        want = { id: confirming, ctl: "remove" };
        confirming = null;
        redraw();
      }
    },
    true,
  );

  // Focus moved elsewhere (the strip, the page, the popup): both close.
  window.addEventListener("blur", () => {
    if (visible() || menuVisible()) send("ov-close");
  });
  document.addEventListener("xb-clear", hide);

  window.xbx = Object.freeze({ show, actions });
})();
