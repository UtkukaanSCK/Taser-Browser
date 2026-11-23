---
name: xb-verify
description: How to build, lint and test X-Browser, and what can and cannot be verified from a Linux cloud container versus Windows. Use before committing any change to src/, ui/, build.rs or Cargo.toml, when asked to "build", "test", "check" or "run clippy", or when a build fails with missing `windows` crate / gdk-3.0 errors.
---

# Verifying an X-Browser change

X-Browser is Windows-only (WebView2 via wry + tao, `windows`/`webview2-com`
under `cfg(windows)`). The crate does **not** compile for a Linux target:
`cargo test` / `cargo build` on Linux fail (missing `gdk-3.0`, then
unresolved `windows::...`). Do not try to "fix" that; it is by design.

## In a Linux container (cloud session)

Cross-check against the Windows target. This type-checks everything,
including `#[cfg(test)]` code and the `selftest` feature, with no linker:

```bash
rustup target add x86_64-pc-windows-msvc
```

```bash
cargo clippy --target x86_64-pc-windows-msvc --all-targets
```

```bash
cargo clippy --target x86_64-pc-windows-msvc --all-targets --features selftest
```

Both must finish with no warnings (the repo is warning-clean). First run
takes ~1 min; later runs a few seconds.

What this **cannot** do: run unit tests (`cargo test` links and executes on
Windows only), run the in-app self-test, measure start-up timing, or look at
the UI. Say so plainly in the final report: "type-checked and clippy-clean
for x86_64-pc-windows-msvc; unit tests and selftest not run (need Windows)".
Never claim tests passed.

## On Windows (the user's machine)

The full list from the README's "Checks" section:

```bash
cargo build
cargo clippy --all-targets
cargo clippy --all-targets --features selftest
cargo test
```

In-app self-test (needs network; writes `selftest.log` + PNGs):

```bash
XB_SELFTEST_DIR=selftest-out cargo run --release --features selftest
```

Pass = last line `DONE failures=0 checks=<EXPECTED_CHECKS>`. See the
`xb-selftest` skill for adding steps.

## UI stylesheet

`ui/dist/app.css` is checked in and embedded with `include_str!`. After
changing classes in `ui/index.html`, `ui/overlay.html`, `ui/js/*.js` or
`ui/input.css`, rebuild it with the Tailwind **v4.3.5** standalone CLI
(kept in the git-ignored `tools/`):

```bash
./tools/tailwindcss -i ui/input.css -o ui/dist/app.css --minify
```

If the CLI is not available (e.g. downloading binaries is not permitted in
the session), do not hand-edit the minified CSS: prefer classes already
present in `ui/dist/app.css` (grep it), and tell the user the CSS must be
rebuilt before the commit is final. A new JS file under `ui/js/` must also
get an `@source` line in `ui/input.css`, or its classes are dropped.

## Quick sanity greps before committing

- New `ui/js/*.js` file: `include_str!` + served in `src/ui.rs`, `<script>`
  in the HTML page, `@source` in `ui/input.css`.
- `EXPECTED_CHECKS` in `src/selftest.rs` matches the PASS lines added/removed.
- README sections touched by the behaviour (see `xb-commit-and-docs`).
