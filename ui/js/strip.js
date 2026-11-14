"use strict";
// The tab list: one element per tab, kept in state order, with a roving
// tabindex keyboard model. Tabs that do not fit scroll sideways (wheel,
// arrows, dragging); a tab can be dragged to a new place. Loaded after core.js.
(() => {
  const { $, send } = ui;

  const zone = $("tabs");
  const tabsEl = $("tablist");
  const left = $("tab-left");
  const right = $("tab-right");
  const tpl = $("tab-tpl").content.firstElementChild;

  /** id -> { el, title, icon, audio, close, iconUrl } */
  const nodes = new Map();

  const label = (t) => t.title || t.url || "Yeni sekme";

  // The only icon shape Rust sends (native/favicon.rs): a base64 PNG. Anything
  // else is dropped, so the <img> can never load from the network.
  const ICON = /^data:image\/png;base64,[A-Za-z0-9+/]+={0,2}$/;

  /** Drag in progress: { el, id, from, x0, pointer, moved, mids }. */
  let drag = null;
  const DRAG_START = 5;

  // What the title and accessible name add for a tab's sound state, so it
  // is told even where the tab is too narrow for its sound button.
  const soundNote = (t) =>
    t.audio ? (t.muted ? " (ses çalıyor, sesi kapalı)" : " (ses çalıyor)") : t.muted ? " (sesi kapalı)" : "";

  function tabNode(id) {
    const el = tpl.cloneNode(true);
    el.dataset.id = id;
    const node = {
      el,
      title: el.querySelector(".title"),
      icon: el.querySelector(".icon"),
      audio: el.querySelector(".audio"),
      close: el.querySelector(".close"),
      iconUrl: null,
    };
    el.addEventListener("mousedown", (e) => {
      if (e.button === 0 && !e.target.closest("button")) send("activate:" + id);
    });
    el.addEventListener("keydown", (e) => tabKey(e, el, id));
    el.addEventListener("auxclick", (e) => {
      if (e.button === 1) send("close:" + id);
    });
    node.close.addEventListener("click", (e) => {
      e.stopPropagation();
      send("close:" + id);
    });
    node.audio.addEventListener("click", (e) => {
      e.stopPropagation();
      send("mute:" + id);
    });
    nodes.set(id, node);
    return node;
  }

  function setIcon(n, url) {
    const ok = typeof url === "string" && ICON.test(url);
    n.iconUrl = ok ? url : null;
    if (ok) n.icon.src = url;
    else n.icon.removeAttribute("src");
    n.el.toggleAttribute("data-icon", ok);
  }

  // Focus without letting the browser scroll anything but the tab list.
  function focusTab(el) {
    el.focus({ preventScroll: true });
    reveal(el);
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
      case "Delete": {
        e.preventDefault();
        // A held key auto-repeats on the neighbour focused below: one press, one close.
        if (e.repeat) return;
        // Closing an inactive tab leaves focus on its neighbour. Closing the active
        // tab makes Rust move focus on to the new page (or the address field of a
        // blank tab) once the state arrives. Closing the last tab quits the app.
        const next = tabs[i + 1] || tabs[i - 1];
        if (next) focusTab(next);
        send("close:" + id);
        return;
      }
      default:
        return;
    }
    e.preventDefault();
    focusTab(to);
  }

  // Scrolls the list just enough to show `el` whole.
  function reveal(el) {
    const l = el.offsetLeft;
    const r = l + el.offsetWidth;
    if (l < tabsEl.scrollLeft) tabsEl.scrollLeft = l;
    else if (r > tabsEl.scrollLeft + tabsEl.clientWidth) tabsEl.scrollLeft = r - tabsEl.clientWidth;
  }

  // Overflow mode: the arrows show while the tabs (at their minimum width) do
  // not fit. Leaving it is measured as if the arrows were gone, so showing them
  // can never be what makes the tabs fit again (no flip-flopping).
  function fit() {
    const on = zone.hasAttribute("data-overflow");
    const spare = on ? left.offsetWidth + right.offsetWidth : 0;
    const over = tabsEl.scrollWidth > tabsEl.clientWidth + spare + 1;
    if (over !== on) zone.toggleAttribute("data-overflow", over);
    arrows();
  }

  function arrows() {
    const max = tabsEl.scrollWidth - tabsEl.clientWidth;
    left.disabled = tabsEl.scrollLeft <= 0;
    right.disabled = tabsEl.scrollLeft >= max - 1;
  }

  // One click moves by a page of tabs, at least one tab.
  function page(dir) {
    const tab = tabsEl.firstElementChild ? tabsEl.firstElementChild.offsetWidth : 36;
    const step = Math.max(tab, Math.floor(tabsEl.clientWidth / tab) * tab);
    tabsEl.scrollLeft += dir * step;
    arrows();
  }

  left.addEventListener("click", () => page(-1));
  right.addEventListener("click", () => page(1));
  tabsEl.addEventListener("scroll", arrows, { passive: true });
  // A vertical wheel scrolls the tabs sideways (horizontal wheels and
  // touchpads already do).
  tabsEl.addEventListener(
    "wheel",
    (e) => {
      if (!zone.hasAttribute("data-overflow") || e.ctrlKey) return;
      const d = Math.abs(e.deltaY) > Math.abs(e.deltaX) ? e.deltaY : e.deltaX;
      if (!d) return;
      e.preventDefault();
      tabsEl.scrollLeft += e.deltaMode === 1 ? d * 16 : d;
    },
    { passive: false },
  );
  new ResizeObserver(() => {
    if (drag) drag.mids = null;
    fit();
    const sel = tabsEl.querySelector('[aria-selected="true"]');
    if (sel) reveal(sel);
  }).observe(tabsEl);

  // Dragging a tab: past a few pixels it follows the pointer and the other
  // tabs make room (the element itself is moved in the DOM); on release the
  // new position goes to Rust, whose next state confirms or undoes it.
  // Escape, a cancelled pointer or lost capture put the old order back.
  tabsEl.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const el = e.target.closest('[role="tab"]');
    if (!el || e.target.closest("button")) return;
    // A drag whose release never arrived is stale: this press replaces it.
    if (drag) stopDrag(false);
    drag = { el, id: el.dataset.id, from: [...tabsEl.children].indexOf(el), x0: e.clientX, pointer: e.pointerId, moved: false, mids: null };
  });
  tabsEl.addEventListener("pointermove", (e) => {
    if (!drag || e.pointerId !== drag.pointer) return;
    if (!drag.moved) {
      if (Math.abs(e.clientX - drag.x0) < DRAG_START) return;
      drag.moved = true;
      drag.el.setAttribute("data-dragging", "");
      tabsEl.setPointerCapture(e.pointerId);
    }
    // Swap with a neighbour once the pointer passes its middle. Tabs all
    // have the same width, so the middles (by DOM position) are measured
    // once per scroll position, not after every move.
    if (!drag.mids || drag.mids.scroll !== tabsEl.scrollLeft) drag.mids = middles();
    const { at } = drag.mids;
    const focused = document.activeElement;
    let i = [...tabsEl.children].indexOf(drag.el);
    const from = i;
    while (i > 0 && e.clientX < at[i - 1]) i--;
    while (i < at.length - 1 && e.clientX > at[i + 1]) i++;
    if (i !== from) {
      const kids = tabsEl.children;
      tabsEl.insertBefore(drag.el, i < from ? kids[i] : kids[i].nextSibling);
      // A moved element loses keyboard focus; give it back.
      if (focused && focused !== document.activeElement && focused.isConnected) focused.focus({ preventScroll: true });
    }
    // Near an edge, scroll toward it.
    const r = tabsEl.getBoundingClientRect();
    if (e.clientX < r.left + 16) tabsEl.scrollLeft -= 8;
    else if (e.clientX > r.right - 16) tabsEl.scrollLeft += 8;
  });
  // Middle of the tab at each strip position, in client coordinates.
  function middles() {
    return {
      scroll: tabsEl.scrollLeft,
      at: [...tabsEl.children].map((el) => {
        const b = el.getBoundingClientRect();
        return b.left + b.width / 2;
      }),
    };
  }
  // Ends the drag. `drop`: the user let go, so the new place goes to Rust;
  // otherwise it is cancelled. Either way the DOM keeps the dragged order
  // only while Rust's answer is on its way (Rust answers every move,
  // refused ones too, with a fresh state); with no move sent the state's
  // order comes back right away, so a dropped tab never snaps back first.
  function stopDrag(drop) {
    const d = drag;
    drag = null;
    if (!d.moved) return;
    d.el.removeAttribute("data-dragging");
    const to = [...tabsEl.children].indexOf(d.el);
    if (drop && to !== d.from && to >= 0) send(`move:${d.id}:${to}`);
    else ui.renderTabs();
    if (tabsEl.hasPointerCapture(d.pointer)) tabsEl.releasePointerCapture(d.pointer);
  }
  const endDrag = (e, drop) => {
    if (drag && e.pointerId === drag.pointer) stopDrag(drop);
  };
  tabsEl.addEventListener("pointerup", (e) => endDrag(e, true));
  tabsEl.addEventListener("pointercancel", (e) => endDrag(e, false));
  // Capture taken away (focus change, another window) is no drop. Capture
  // is only taken once a drag moves; a fresh press is not ended by the late
  // notice of an earlier capture.
  tabsEl.addEventListener("lostpointercapture", (e) => {
    if (drag && drag.moved) endDrag(e, false);
  });
  // Escape cancels only while the UI has keyboard focus: pressing an inactive
  // web tab activates it and Rust moves focus into its page (known limit).
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape" || !drag || !drag.moved) return;
      e.preventDefault();
      e.stopPropagation();
      stopDrag(false);
    },
    true,
  );

  let shown = { active: null, count: 0 };

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
      // The title is the accessible name; the buttons' own labels stay out of
      // it. The sound state is part of it: narrow tabs show no sound button.
      const name = text + soundNote(t);
      n.el.title = name;
      n.el.setAttribute("aria-label", name);
      n.el.setAttribute("aria-selected", t.id === state.active ? "true" : "false");
      n.el.tabIndex = t.id === stop ? 0 : -1;
      n.el.toggleAttribute("data-loading", t.loading);
      // `icon` comes only when it changed: absent = keep, null = none.
      if (t.icon !== undefined) setIcon(n, t.icon);
      n.el.toggleAttribute("data-audio", !!t.audio);
      n.el.toggleAttribute("data-muted", !!t.muted);
      const sound = t.muted ? "Sesi aç" : "Sesi kapat";
      if (n.audio.title !== sound) {
        n.audio.title = sound;
        n.audio.setAttribute("aria-label", sound);
      }
      // Keep DOM order equal to state order without touching nodes already in
      // place. While a tab is being dragged the user's order stands.
      const want = prev ? prev.nextSibling : tabsEl.firstChild;
      if (want !== n.el && (!drag || !drag.moved || !n.el.isConnected)) tabsEl.insertBefore(n.el, want);
      prev = n.el;
    }
    // Tabs came, went or resized: measure the middles again.
    if (drag) drag.mids = null;
    fit();
    // Bring the active tab into view when it changes or tabs come and go.
    if (state.active !== shown.active || state.tabs.length !== shown.count) {
      const sel = nodes.get(state.active);
      if (sel && !drag) reveal(sel.el);
      shown = { active: state.active, count: state.tabs.length };
      arrows();
    }
  };
})();
