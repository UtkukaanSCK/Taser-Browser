"use strict";
// X-Browser UI strip. Holds no state of its own: Rust pushes the full state
// with xb.render(), and every user action is sent back as a short IPC command.
// Page titles and URLs are untrusted, so they only ever go through
// textContent / value / title, never innerHTML.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const tabsEl = $("tablist");
  const newTab = $("new-tab");
  const addr = $("address");
  const addrBox = $("address-box");
  const back = $("back");
  const fwd = $("forward");
  const reload = $("reload");
  const maxBtn = $("max");
  const resizer = $("resizer");
  const loadbar = $("loadbar");
  const tpl = $("tab-tpl").content.firstElementChild;

  let state = { active: null, tabs: [] };
  /** id -> { el, title, close } */
  const nodes = new Map();
  /** The user is working in the address bar (focused it, or typed into it). */
  let editing = false;
  let blockedTimer = 0;

  const activeTab = () => state.tabs.find((t) => t.id === state.active);
  const label = (t) => t.title || t.url || "Yeni sekme";

  function tabNode(id) {
    const el = tpl.cloneNode(true);
    el.dataset.id = id;
    const node = { el, title: el.querySelector(".title"), close: el.querySelector(".close") };
    el.addEventListener("mousedown", (e) => {
      if (e.button === 0 && !node.close.contains(e.target)) send("activate:" + id);
    });
    el.addEventListener("keydown", (e) => tabKey(e, el, id));
    el.addEventListener("auxclick", (e) => {
      if (e.button === 1) send("close:" + id);
    });
    node.close.addEventListener("click", (e) => {
      e.stopPropagation();
      send("close:" + id);
    });
    nodes.set(id, node);
    return node;
  }

  // Tab keyboard model: roving tabindex (the selected tab is the tab stop),
  // arrows/Home/End move focus, Enter/Space activate, Delete closes.
  function tabKey(e, el, id) {
    if (e.target !== el || e.isComposing || e.ctrlKey || e.altKey || e.metaKey) return;
    const tabs = [...tabsEl.children];
    const i = tabs.indexOf(el);
    let to = null;
    switch (e.key) {
      case "ArrowRight":
        to = tabs[(i + 1) % tabs.length];
        break;
      case "ArrowLeft":
        to = tabs[(i - 1 + tabs.length) % tabs.length];
        break;
      case "Home":
        to = tabs[0];
        break;
      case "End":
        to = tabs[tabs.length - 1];
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        if (!e.repeat) send("activate:" + id);
        return;
      case "Delete":
        e.preventDefault();
        // A held key auto-repeats on the neighbour focused below: one press, one close.
        if (e.repeat) return;
        // Closing an inactive tab leaves focus on its neighbour. Closing the active
        // tab makes Rust move focus on to the new page (or the address field of a
        // blank tab) once the state arrives. Closing the last tab quits the app.
        (tabs[i + 1] || tabs[i - 1])?.focus();
        send("close:" + id);
        return;
      default:
        return;
    }
    e.preventDefault();
    to.focus();
  }

  function renderTabs() {
    const seen = new Set(state.tabs.map((t) => t.id));
    // Closed tabs go first: a stale node left in place would make the ordering
    // below move every tab after it, and a moved element loses keyboard focus.
    for (const [id, n] of nodes) {
      if (!seen.has(id)) {
        n.el.remove();
        nodes.delete(id);
      }
    }
    // The one tab stop: the active tab (or the first tab while none is active).
    const stop = seen.has(state.active) ? state.active : state.tabs[0]?.id;
    let prev = null;
    for (const t of state.tabs) {
      const n = nodes.get(t.id) || tabNode(t.id);
      const text = label(t);
      if (n.title.textContent !== text) n.title.textContent = text;
      n.el.title = text;
      // The title is the accessible name; the close button's own label stays out of it.
      n.el.setAttribute("aria-label", text);
      n.el.setAttribute("aria-selected", t.id === state.active ? "true" : "false");
      n.el.tabIndex = t.id === stop ? 0 : -1;
      n.el.toggleAttribute("data-loading", t.loading);
      // Keep DOM order equal to state order without touching nodes already in place.
      const want = prev ? prev.nextSibling : tabsEl.firstChild;
      if (want !== n.el) tabsEl.insertBefore(n.el, want);
      prev = n.el;
    }
  }

  // Tabs shrink with flexbox and shed their contents by their own width
  // (container queries in the tab template), so nothing is measured here.

  // The page's URL never replaces what the user is editing; only leaving the
  // field (blur, Escape) or switching tabs does.
  function syncAddress(force) {
    // Both checks: Escape clears `editing` while the field keeps focus, and a
    // stale `editing` must never freeze a field that has lost focus.
    if (!force && editing && document.activeElement === addr) return;
    const t = activeTab();
    const url = t ? t.url : "";
    if (addr.value !== url) addr.value = url;
  }

  function render(next, maximized) {
    const switched = next.active !== state.active;
    state = next;
    renderTabs();
    const t = activeTab();
    back.disabled = !(t && t.back);
    fwd.disabled = !(t && t.fwd);
    const loading = !!(t && t.loading);
    reload.toggleAttribute("data-loading", loading);
    reload.title = loading ? "Durdur" : "Yenile (Ctrl+R)";
    reload.setAttribute("aria-label", loading ? "Durdur" : "Yenile");
    loadbar.toggleAttribute("data-on", loading);
    maxBtn.toggleAttribute("data-max", maximized);
    maxBtn.title = maximized ? "Önceki boyutuna getir" : "Ekranı kapla";
    resizer.hidden = maximized;
    syncAddress(switched);
  }

  // Address bar.
  addr.addEventListener("focus", () => {
    editing = true;
    addr.select();
  });
  addr.addEventListener("input", () => {
    editing = true;
  });
  addr.addEventListener("blur", () => {
    editing = false;
    syncAddress(true);
  });
  addr.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if (e.key === "Enter") {
      e.preventDefault();
      const text = addr.value.trim();
      if (!text) return;
      // The text stays while the field has focus: if Rust refuses it (xb.blocked())
      // the user can fix it. A started navigation moves focus to the page, and
      // the resulting blur shows the page's URL.
      send("navigate:" + text);
    } else if (e.key === "Escape") {
      e.preventDefault();
      editing = false;
      syncAddress(true);
      send("focus-content");
    }
  });

  // Toolbar.
  back.addEventListener("click", () => send("back"));
  fwd.addEventListener("click", () => send("forward"));
  reload.addEventListener("click", () => send(reload.hasAttribute("data-loading") ? "stop" : "reload"));
  newTab.addEventListener("click", () => send("new"));

  // Window chrome. Empty strip space drags the window; double-click maximizes.
  for (const el of [$("tabs"), $("strip")]) {
    el.addEventListener("mousedown", (e) => {
      if (e.button !== 0 || e.target !== el) return;
      e.preventDefault();
      send(e.detail === 2 ? "max" : "drag");
    });
  }
  for (const el of resizer.children) {
    el.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      send("resize:" + el.dataset.resize);
    });
  }
  $("min").addEventListener("click", () => send("min"));
  maxBtn.addEventListener("click", () => send("max"));
  $("close").addEventListener("click", () => send("quit"));

  // Middle-click autoscroll and text-selection drags have no place in the strip.
  document.addEventListener("mousedown", (e) => {
    if (e.button === 1) e.preventDefault();
  });

  window.xb = Object.freeze({
    render,
    focusAddress() {
      editing = true;
      addr.focus();
      addr.select();
    },
    blocked() {
      addrBox.setAttribute("data-blocked", "");
      clearTimeout(blockedTimer);
      blockedTimer = setTimeout(() => addrBox.removeAttribute("data-blocked"), 900);
    },
  });

  send("ready");
})();
