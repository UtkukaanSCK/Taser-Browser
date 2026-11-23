---
name: xb-ui-command
description: How to add or change a UI → Rust IPC command, a strip/toolbar control, or an overlay page (menu, settings, panels, questions) in X-Browser while keeping the trust model intact. Use when touching src/commands.rs, src/overlay.rs, src/ui.rs, ui/*.html or ui/js/*.js, or when adding any button, panel, shortcut or Turkish UI text.
---

# UI commands, overlay pages and the trust model

## The model (do not weaken it)

- Three kinds of webview in one tao window: the **UI strip** (`xb://ui/index.html`),
  the **overlay** (`xb://ui/overlay.html`, popups/panels), and **content tabs**.
  Extension popups are a fourth (`src/popup.rs`).
- UI and overlay live in their own WebView2 profile (`XbUi`); content tabs and
  extensions can never reach their IPC. IPC from any other origin is rejected
  (`ui::is_ui_origin`). Content tabs must never load `xb.ui` (`ui::is_ui_url`).
- **Rust decides, JS draws.** JS files keep no state of their own: Rust pushes
  a view (`serde_json::to_string(&view)` → `evaluate_script("xbX.show(...)")`)
  and JS reports user actions back as short command strings. Never build JS
  by interpolating unescaped text; always go through `serde_json`.
- Strict CSP (`ui::CSP`): no inline script/style, no network, no eval, no
  frames, no forms. Use DOM APIs + `textContent`, never `innerHTML` with data.
- `UiCommand` and `Shortcut` (`src/commands.rs`) are **closed sets**. Anything
  unknown or malformed parses to `None`. Shortcuts are computed in Rust from
  native key events and carry no data. Shortcuts are Ctrl-without-Alt only
  (AltGr on Turkish layouts must keep typing).

## Adding a command

1. Put the module's own enum next to its logic (e.g. `SettingsCmd` in
   `src/settings.rs`) with `NAMES: &[&str]` and `parse(name, arg) -> Option<Self>`.
   Wire format is `name` or `name:arg`; prefix names per module (`st-*`, …).
2. Bound every argument (length in chars, numeric ranges, allowed ids). Paths
   are only accepted where already documented (`st-folder`: absolute, no UNC).
3. Add the variant to `UiCommand`, a line in `UiCommand::parse`, and a branch
   in `App::handle` (`src/app.rs`) that calls the module's `on_command`.
4. Unit tests in `src/commands.rs` `mod tests`: the good forms parse, and
   malformed / oversized / unknown ones return `None`.

## Adding an overlay page

1. A variant in `overlay::Mode` (with its doc comment naming the owner
   module); decide whether it takes the keyboard (`Mode` helpers) and what
   closes it (Esc, focus loss, tab switch, window move/resize, navigation).
2. Markup in `ui/overlay.html`; a `ui/js/<name>.js` exposing one frozen global
   (`window.xbFoo = Object.freeze({ show, ... })`), `"use strict"`, IIFE.
3. Register the JS: `include_str!` + route in `src/ui.rs`, `<script>` tag,
   `@source "./js/<name>.js";` in `ui/input.css`, rebuild CSS (`xb-verify`).
4. Keyboard first: every control reachable and operable by keys; focus
   returns sensibly after redraws. Native `<select>` dropdowns close the
   overlay on focus loss — use radio groups / segmented controls instead.
5. State must never be shown by colour alone; announce changes to screen
   readers where the strip does (see `ui/js/security.js`).

## UI text

- All user-facing text is **Turkish**, short and plain (e.g. "Ekle",
  "İptal", "Kaldır", "Yeniden dene", "Bu sitenin verilerini sil"). Questions
  that delete or install always ask first. Code, comments, docs and commit
  messages stay in English.
- Error pages and messages never show Edge/WebView2 branding.
- Message tables live in the JS page keyed by a code Rust sends
  (e.g. `MESSAGES["folder-bad"]` in `ui/js/settings.js`); Rust sends codes,
  not sentences, unless the text is data (a host, an extension name).

## Done when

Clippy-clean for the Windows target (both feature sets), parse tests added,
README updated if behaviour or a shortcut changed, selftest step added for
anything a script can drive (`xb-selftest`).
