"use strict";
// X-Browser UI strip. Holds no state of its own: Rust pushes the full state
// with xb.render(), and every user action is sent back as a short IPC command.
// Page titles and URLs are untrusted, so they only ever go through
// textContent / value / title, never innerHTML.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const tabsEl = $("tabs");
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
  /** True while the user has typed into the address bar since it was last synced. */
  let typed = false;
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

  function renderTabs() {
    const seen = new Set();
    let prev = null;
    for (const t of state.tabs) {
      seen.add(t.id);
      const n = nodes.get(t.id) || tabNode(t.id);
      const text = label(t);
      if (n.title.textContent !== text) n.title.textContent = text;
      n.el.title = text;
      n.el.setAttribute("aria-selected", t.id === state.active ? "true" : "false");
      n.el.toggleAttribute("data-loading", t.loading);
      // Keep DOM order equal to state order without touching nodes already in place.
      const want = prev ? prev.nextSibling : tabsEl.firstChild;
      if (want !== n.el) tabsEl.insertBefore(n.el, want);
      prev = n.el;
    }
    for (const [id, n] of nodes) {
      if (!seen.has(id)) {
        n.el.remove();
        nodes.delete(id);
      }
    }
    if (prev ? prev.nextSibling !== newTab : tabsEl.firstChild !== newTab) {
      tabsEl.insertBefore(newTab, prev ? prev.nextSibling : tabsEl.firstChild);
    }
    fitTabs();
  }

  // Tabs shrink with flexbox; once they get narrow, only the active one keeps
  // its close button so the title stays readable.
  function fitTabs() {
    const n = state.tabs.length || 1;
    const per = (tabsEl.clientWidth - newTab.offsetWidth - 32) / n;
    const narrow = per < 76;
    for (const t of state.tabs) {
      const node = nodes.get(t.id);
      if (node) node.close.hidden = narrow && t.id !== state.active;
    }
  }

  function syncAddress(force) {
    const t = activeTab();
    const url = t ? t.url : "";
    if ((force || !typed) && addr.value !== url) addr.value = url;
  }

  function render(next, maximized) {
    const switched = next.active !== state.active;
    state = next;
    if (switched) typed = false;
    renderTabs();
    const t = activeTab();
    back.disabled = !(t && t.back);
    fwd.disabled = !(t && t.fwd);
    const loading = !!(t && t.loading);
    reload.toggleAttribute("data-loading", loading);
    reload.title = loading ? "Durdur (Esc)" : "Yenile (Ctrl+R)";
    reload.setAttribute("aria-label", loading ? "Durdur" : "Yenile");
    loadbar.toggleAttribute("data-on", loading);
    maxBtn.toggleAttribute("data-max", maximized);
    maxBtn.title = maximized ? "Önceki boyutuna getir" : "Ekranı kapla";
    resizer.hidden = maximized;
    syncAddress(false);
  }

  // Address bar.
  addr.addEventListener("focus", () => addr.select());
  addr.addEventListener("input", () => {
    typed = true;
  });
  addr.addEventListener("blur", () => {
    typed = false;
    syncAddress(true);
  });
  addr.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    if (e.key === "Enter") {
      e.preventDefault();
      const text = addr.value.trim();
      if (!text) return;
      typed = false;
      send("navigate:" + text);
    } else if (e.key === "Escape") {
      e.preventDefault();
      typed = false;
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
  for (const el of [tabsEl, $("strip")]) {
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

  window.addEventListener("resize", fitTabs);
  // Middle-click autoscroll and text-selection drags have no place in the strip.
  document.addEventListener("mousedown", (e) => {
    if (e.button === 1) e.preventDefault();
  });

  window.xb = Object.freeze({
    render,
    focusAddress() {
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
