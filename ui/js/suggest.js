"use strict";
// Address-bar suggestions. The list itself is drawn by the overlay webview,
// which Rust places under the address box. The address input keeps focus
// throughout: while the list is open, ↑/↓/Enter/Esc go to Rust instead of
// the input (Rust says when the list opens or closes with xbSuggest.set).
(() => {
  const { $, send } = ui;
  const addr = $("address");
  const box = $("address-box");
  /** Longer text gets no suggestions (Rust refuses over 8 KiB). */
  const MAX_BYTES = 8000;
  const utf8 = new TextEncoder();

  let open = false;
  /** What the user typed; ↑/↓ show a suggestion's address in its place. */
  let typed = "";

  function close() {
    open = false;
    send("suggest-close");
  }

  addr.addEventListener("input", () => {
    typed = addr.value;
    if (!typed.trim() || utf8.encode(typed).length > MAX_BYTES) {
      close();
      return;
    }
    const r = box.getBoundingClientRect();
    send(`suggest:${Math.max(0, Math.round(r.left))}:${Math.max(0, Math.round(r.width))}:${typed}`);
  });

  // Capture phase on the document: runs before core.js's own Enter/Escape.
  document.addEventListener(
    "keydown",
    (e) => {
      if (!open || e.target !== addr || e.isComposing) return;
      let key;
      if (e.key === "ArrowDown") key = "down";
      else if (e.key === "ArrowUp") key = "up";
      else if (e.key === "Enter") key = "enter";
      else if (e.key === "Escape") key = "esc";
      else return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (key === "esc") {
        // First Esc: back to the typed text, list closed; focus stays.
        addr.value = typed;
        close();
      } else {
        send("suggest-key:" + key);
      }
    },
    true,
  );

  addr.addEventListener("blur", () => {
    if (open) close();
  });

  window.xbSuggest = Object.freeze({
    /** The list is open (`on`); `text`, if a string, goes into the input. */
    set(on, text) {
      open = !!on;
      if (typeof text === "string") addr.value = text;
    },
  });
})();
