"use strict";
// The main menu in the overlay webview. Rust decides when it is shown and
// where (xbm.show); this page draws it, moves the highlight with the keys and
// reports the choice. Esc is the overlay's own (overlay.js: "ov-esc").
(() => {
  const send = (msg) => window.ipc.postMessage(msg);
  const menu = document.getElementById("menu");
  const zoom = document.getElementById("m-zoom");
  const stops = [...menu.querySelectorAll("[data-stop]")];

  const visible = () => !menu.hidden;

  function show(percent) {
    menu.hidden = false;
    setZoom(percent);
    stops[0].focus();
  }

  function setZoom(percent) {
    const p = Number.isSafeInteger(percent) && percent > 0 ? percent : 100;
    zoom.textContent = "%" + p;
    zoom.setAttribute("aria-label", `Yakınlaştırma yüzde ${p}, sıfırla`);
  }

  function hide() {
    menu.hidden = true;
  }

  function move(delta) {
    const at = stops.indexOf(document.activeElement);
    const next = at < 0 ? (delta > 0 ? 0 : stops.length - 1) : (at + delta + stops.length) % stops.length;
    stops[next].focus();
  }

  for (const b of menu.querySelectorAll("button[data-item]")) {
    b.addEventListener("click", () => send("menu-pick:" + b.dataset.item));
  }

  document.addEventListener("keydown", (e) => {
    if (!visible() || e.isComposing) return;
    const plain = !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;
    let handled = true;
    if (plain && e.key === "ArrowDown") move(1);
    else if (plain && e.key === "ArrowUp") move(-1);
    else if (plain && e.key === "Home") stops[0].focus();
    else if (plain && e.key === "End") stops[stops.length - 1].focus();
    else if (e.key === "Tab" && !e.ctrlKey && !e.altKey) move(e.shiftKey ? -1 : 1);
    else if (plain && document.activeElement === zoom && e.key === "ArrowLeft") send("menu-pick:zoom-out");
    else if (plain && document.activeElement === zoom && e.key === "ArrowRight") send("menu-pick:zoom-in");
    // Ctrl+F and Ctrl+P: WebView2 would answer them in this page.
    else if (e.ctrlKey && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") send("menu-pick:find");
    else if (e.ctrlKey && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "p") send("menu-pick:print");
    else handled = false;
    if (handled) e.preventDefault();
  });

  // Focus moved elsewhere (the strip, the page): the menu closes.
  window.addEventListener("blur", () => {
    if (visible()) send("ov-close");
  });
  document.addEventListener("xb-clear", hide);

  window.xbm = Object.freeze({ show, zoom: setZoom });
})();
