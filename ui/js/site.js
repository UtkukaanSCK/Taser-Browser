"use strict";
// The site information panel in the overlay webview (src/site_info.rs). Rust
// sends what to draw (xbSite.show) and acts on the choices sent back; this
// page writes everything as text and decides nothing.
(() => {
  const $ = (id) => document.getElementById(id);
  const send = (msg) => window.ipc.postMessage(msg);

  const page = $("site");
  const host = $("si-host");
  const conn = $("si-conn");
  const list = $("si-perms");
  const status = $("si-status");
  const actions = $("si-actions");
  const confirmRow = $("si-confirm");
  const clear = $("si-clear");

  const str = (v) => (typeof v === "string" ? v : "");
  const int = (v) => (Number.isInteger(v) ? v : -1);
  const visible = () => !page.hidden;

  function hideConfirm() {
    confirmRow.hidden = true;
    actions.hidden = false;
  }

  // A row: the permission's name, then its three choices as one radio group
  // drawn as a segmented control (no <select>: its dropdown would be a
  // window of its own, and the panel closes when the overlay loses focus).
  function row(r, i) {
    const kind = int(r.kind);
    const state = [0, 1, 2].includes(r.state) ? r.state : 0;
    const li = document.createElement("li");
    li.className = "flex h-9 items-center gap-2 px-4";
    const name = document.createElement("span");
    name.id = "si-name-" + i;
    name.className = "mr-auto min-w-0 truncate";
    name.textContent = str(r.name);
    const group = document.createElement("div");
    group.setAttribute("role", "radiogroup");
    group.setAttribute("aria-labelledby", name.id);
    group.dataset.kind = String(kind);
    group.className = "flex shrink-0 overflow-hidden rounded-md border border-line";
    for (const [value, label] of [[0, str(r.default) || "Sor"], [1, "İzin ver"], [2, "Engelle"]]) {
      const l = document.createElement("label");
      l.className = "flex border-line not-first:border-l";
      const input = document.createElement("input");
      input.type = "radio";
      input.name = "si-k-" + kind;
      input.value = String(value);
      input.checked = value === state;
      input.className = "peer sr-only";
      input.addEventListener("change", () => input.checked && send("si-perm:" + kind + ":" + value));
      const text = document.createElement("span");
      text.textContent = label;
      text.className =
        "flex h-6 items-center px-2 text-[12px] text-muted hover:bg-hover peer-checked:bg-hover peer-checked:font-semibold peer-checked:text-ink peer-checked:shadow-[inset_0_-2px_0_var(--color-accent)] peer-focus-visible:outline-2 peer-focus-visible:-outline-offset-2 peer-focus-visible:outline-accent";
      l.append(input, text);
      group.append(l);
    }
    li.append(name, group);
    return li;
  }

  /** `keep`: an update of the panel already up (focus and the question stay). */
  function show(data, keep) {
    const d = data && typeof data === "object" ? data : {};
    page.hidden = false;
    host.textContent = str(d.host);
    host.title = str(d.origin);
    conn.textContent = str(d.conn);
    // The choice that has the focus is rebuilt; the focus goes to its group's
    // choice again.
    const active = document.activeElement;
    const focused = keep && active && active.type === "radio" && list.contains(active) ? active.name : null;
    const rows = Array.isArray(d.rows) ? d.rows : [];
    list.replaceChildren(...rows.map(row));
    status.textContent = str(d.status);
    if (focused) {
      const again = list.querySelector(`input[name="${CSS.escape(focused)}"]:checked`);
      if (again) again.focus();
    }
    if (!keep) {
      hideConfirm();
      const first = list.querySelector("input:checked");
      (first || clear).focus();
    }
  }

  function hide() {
    page.hidden = true;
    hideConfirm();
  }

  clear.addEventListener("click", () => {
    actions.hidden = true;
    confirmRow.hidden = false;
    $("si-no").focus();
  });
  $("si-yes").addEventListener("click", () => {
    hideConfirm();
    send("si-clear");
    clear.focus();
  });
  $("si-no").addEventListener("click", () => {
    hideConfirm();
    clear.focus();
  });

  // Esc first backs out of the question; only then it closes the panel (the
  // overlay's own handler, which hears it after this one).
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape" && visible() && !confirmRow.hidden) {
        e.stopImmediatePropagation();
        e.preventDefault();
        hideConfirm();
        clear.focus();
      }
    },
    true,
  );
  window.addEventListener("blur", () => {
    if (visible()) send("ov-close");
  });
  document.addEventListener("xb-clear", hide);

  window.xbSite = Object.freeze({ show });
})();
