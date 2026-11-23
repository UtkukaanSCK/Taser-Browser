# X-Browser

A light desktop browser in Rust on the system WebView (WebView2), built with
`wry` + `tao` only. **Windows-only**: the crate does not compile for Linux.
The README is the spec: read the section for what you are changing first.

## Checking a change

From a Linux container (cloud sessions), type-check for Windows:

```bash
cargo clippy --target x86_64-pc-windows-msvc --all-targets
cargo clippy --target x86_64-pc-windows-msvc --all-targets --features selftest
```

Both must be warning-free. `cargo test` and the in-app self-test
(`--features selftest`) run on Windows only. If you could not run them, say
so; never report them as passed. Details: `.claude/skills/xb-verify`.

## Rules that are easy to break

- **Trust model.** UI strip and overlay live in their own WebView2 profile
  (`XbUi`); content tabs and extensions never reach their IPC. `UiCommand`
  and `Shortcut` (`src/commands.rs`) are closed sets: unknown or malformed
  input parses to `None`, every argument is bounded.
- **Rust decides, JS draws.** `ui/js/*.js` keep no state; Rust sends views as
  JSON (`serde_json`), JS reports actions as short command strings. Strict
  CSP: no inline script, no `innerHTML` with data, no network.
- **Profile files are untrusted input.** Write through `src/store.rs`
  (atomic, `.bak`, `.corrupt`); read leniently with size and count caps.
- **Language.** UI text is Turkish; code, comments, docs and commit messages
  are English.
- **Shortcuts** are Ctrl without Alt, so AltGr on Turkish layouts keeps
  typing.
- **The `selftest` feature** builds a fake Web Store and accepts self-signed
  packages: test-only, never distributed. Keep test code behind
  `#[cfg(feature = "selftest")]`; update `EXPECTED_CHECKS` with the checks.
- **Generated files.** `ui/dist/app.css` (Tailwind v4.3.5 from
  `ui/input.css`; a new JS file needs an `@source` line), `assets/*.ico` and
  `docs/media/*.svg` (from their Python scripts) are never hand-edited.
- **Measurements.** Never invent timing or binary sizes, and never claim
  manual checklist steps were done.

## Skills

| Skill | Use for |
| --- | --- |
| `xb-verify` | building, linting, testing; what works from Linux |
| `xb-ui-command` | IPC commands, strip controls, overlay pages, UI text |
| `xb-settings-field` | settings and any persisted profile data |
| `xb-selftest` | adding or fixing self-test steps |
| `xb-commit-and-docs` | commit message style, README sections to update |

## Layout

`src/app.rs` owns state and handles IPC; `src/native/` holds WebView2 calls
`wry` does not expose; `ui/` is the strip (`index.html`) and overlay
(`overlay.html`); `docs/` has per-version design notes and manual
checklists. The full file table is the README's "Layout" section.
