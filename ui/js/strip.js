"use strict";
// The tab list: one element per tab, kept in state order, with a roving
// tabindex keyboard model. Loaded after core.js.
(() => {
  const { $, send } = ui;

  const tabsEl = $("tablist");
  const tpl = $("tab-tpl").content.firstElementChild;

  /** id -> { el, title, close } */
  const nodes = new Map();

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

  ui.renderTabs = function renderTabs() {
    const state = ui.state;
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
  };

  // Tabs shrink with flexbox and shed their contents by their own width
  // (container queries in the tab template), so nothing is measured here.
})();
