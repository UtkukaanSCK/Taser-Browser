"use strict";
// Taser UI strip. Holds no state of its own: Rust pushes the full state
// with xb.render(), and every user action is sent back as a short IPC command.
// Page titles and URLs are untrusted, so they only ever go through
// textContent / value / title, never innerHTML.
//
// Scripts (deferred, in this order): core.js (address bar, toolbar, window
// chrome, xb API), strip.js (the tab list). Classic scripts share their
// top-level lexical scope, which is how they share `ui`.
const ui = {
  $: (id) => document.getElementById(id),
  send: (msg) => window.ipc.postMessage(msg),
  /** The last state pushed by Rust: { active, tabs: [...] }. */
  state: { active: null, tabs: [] },
  /** Draws ui.state.tabs; set by strip.js. */
  renderTabs: () => {},
};

(() => {
  const { $, send } = ui;

  const newTab = $("new-tab");
  const addr = $("address");
  const addrBox = $("address-box");
  const back = $("back");
  const fwd = $("forward");
  const reload = $("reload");
  const maxBtn = $("max");
  const resizer = $("resizer");
  const loadbar = $("loadbar");
  const zoomBtn = $("zoom");
  const zoomLevel = $("zoom-level");
  const downloadsBtn = $("downloads-btn");
  const notice = $("fs-notice");
  const noticeSite = $("fs-notice-site");
  const noticeText = $("fs-notice-text");

  /** The user is working in the address bar (focused it, or typed into it). */
  let editing = false;
  let blockedTimer = 0;

  const activeTab = () => ui.state.tabs.find((t) => t.id === ui.state.active);

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
    const switched = next.active !== ui.state.active;
    ui.state = next;
    ui.renderTabs();
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
    const zoom = t && Number.isInteger(t.zoom) ? t.zoom : 100;
    zoomBtn.hidden = zoom === 100;
    if (zoom !== 100) {
      zoomLevel.textContent = "%" + zoom;
      zoomBtn.title = `Yakınlaştırma %${zoom}. Sıfırla (Ctrl+0)`;
      zoomBtn.setAttribute("aria-label", `Yakınlaştırmayı sıfırla (%${zoom})`);
    }
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
  // Find and print are the page's own keys (WebView2 answers them in a
  // page); typed in the strip they would act on the strip, so ask for them.
  document.addEventListener("keydown", (e) => {
    if (e.isComposing || !e.ctrlKey || e.altKey || e.shiftKey) return;
    const k = e.key.toLowerCase();
    if (k === "f" || k === "p") {
      e.preventDefault();
      send(k === "f" ? "menu-pick:find" : "menu-pick:print");
    }
  });

  // Toolbar.
  back.addEventListener("click", () => send("back"));
  fwd.addEventListener("click", () => send("forward"));
  reload.addEventListener("click", () => send(reload.hasAttribute("data-loading") ? "stop" : "reload"));
  newTab.addEventListener("click", () => send("new"));
  $("home").addEventListener("click", () => send("home"));
  $("menu-btn").addEventListener("click", () => send("menu-toggle"));
  downloadsBtn.addEventListener("click", () => send("menu-pick:downloads"));
  zoomBtn.addEventListener("click", () => {
    if (ui.state.active != null) send("zoom-reset:" + ui.state.active);
  });

  // Window chrome. Empty tab bar and toolbar space drags the window;
  // double-click maximizes.
  for (const el of [$("tabs"), $("tablist"), $("tabbar"), $("toolbar"), $("strip")]) {
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
    /** The downloads button: shown once there was a download; `busy`: one is running. */
    downloads(show, busy) {
      downloadsBtn.hidden = show !== true;
      downloadsBtn.toggleAttribute("data-busy", busy === true);
      downloadsBtn.title = busy === true ? "İndirilenler: indiriliyor (Ctrl+J)" : "İndirilenler (Ctrl+J)";
    },
    /** Fullscreen notice: `site` (or null) and `text`; `notice(null)` hides it. */
    notice(site, text) {
      const on = typeof text === "string";
      noticeSite.textContent = on && typeof site === "string" ? site : "";
      noticeText.textContent = on ? text : "";
      notice.hidden = !on;
      $("strip").inert = on;
    },
  });

  // After every deferred script has run, so strip.js is in place before the
  // first state arrives.
  document.addEventListener("DOMContentLoaded", () => send("ready"));
})();
