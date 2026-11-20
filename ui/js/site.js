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

  function row(r, i) {
    const li = document.createElement("li");
    li.className = "flex h-9 items-center gap-2 px-4";
    const name = document.createElement("span");
    name.id = "si-name-" + i;
    name.className = "mr-auto min-w-0 truncate";
    name.textContent = str(r.name);
    const pick = document.createElement("select");
    pick.dataset.kind = String(int(r.kind));
    pick.setAttribute("aria-labelledby", name.id);
    pick.className = "h-7 w-28 shrink-0 rounded-md border border-line bg-surface px-1 outline-none focus:border-accent";
    for (const [value, label] of [[0, str(r.default) || "Sor"], [1, "İzin ver"], [2, "Engelle"]]) {
      const o = document.createElement("option");
      o.value = String(value);
      o.textContent = label;
      pick.append(o);
    }
    pick.value = String([0, 1, 2].includes(r.state) ? r.state : 0);
    pick.addEventListener("change", () => send("si-perm:" + pick.dataset.kind + ":" + pick.value));
    li.append(name, pick);
    return li;
  }

  /** `keep`: an update of the panel already up (focus and the question stay). */
  function show(data, keep) {
    const d = data && typeof data === "object" ? data : {};
    page.hidden = false;
    host.textContent = str(d.host);
    host.title = str(d.origin);
    conn.textContent = str(d.conn);
    // The select that has the focus is rebuilt as the same select.
    const focused = keep && document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.kind : null;
    const rows = Array.isArray(d.rows) ? d.rows : [];
    list.replaceChildren(...rows.map(row));
    status.textContent = str(d.status);
    if (focused) {
      const again = list.querySelector(`select[data-kind="${CSS.escape(focused)}"]`);
      if (again) again.focus();
    }
    if (!keep) {
      hideConfirm();
      const first = list.querySelector("select");
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
