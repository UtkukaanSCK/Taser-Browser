---
name: xb-selftest
description: How X-Browser's in-app self-test (src/selftest.rs, --features selftest) is structured and how to add, change or remove steps and checks, including keeping EXPECTED_CHECKS right. Use when a change adds behaviour a script can drive, when a selftest FAILs, or when editing src/selftest.rs.
---

# The in-app self-test

`src/selftest.rs` (~7k lines) drives the real app: UI DOM events → IPC,
shortcuts through `Shortcut::from_key` + `App::handle`, page events, and
captures PNGs. Built only with `--features selftest`; that build uses a fake
Web Store (`webstore::fake`) and `crx::Policy::SelfSigned`, so it is **never
distributed**. Keep test-only code behind `#[cfg(feature = "selftest")]`.

## Structure

- `DELAYS: &[u64]` — one entry per step, the wait *before* that step, with a
  `// N description` comment. Network steps get ~2.5–3.5 s, local ones
  300–800 ms.
- `pub fn step(app, n)` — a big `match n { ... }`. A step usually checks the
  result of the previous step's action, then starts the next action.
- `check(name, ok, detail)` logs `PASS name` or `FAIL name -- detail`. Names
  are kebab-case, unique and say the expected behaviour
  (`ext-disabled-lets-the-ad-script-through`).
- Async results (script callbacks) call `check` from the callback; one that
  never returns is caught because the PASS count falls short.
- `EXPECTED_CHECKS` — the exact number of PASS lines of a full passing run.
  A mismatch logs an `all-expected-checks-ran` FAIL.
- Test extensions live in `test-assets/selftest-ext/`.

## Adding steps

1. Append new steps at the end (renumbering the middle breaks references in
   commit history and docs). Add a `DELAYS` entry per step, with comment.
2. Each check: one behaviour, a detail string that says what was seen.
3. Count the new `check(...)` calls that run on a passing path and update
   `EXPECTED_CHECKS`. Conditional checks count only if they always run.
4. Never use real keyboard/mouse input to the desktop or click WebView2's
   native dialogs; things that need a human go to
   `docs/v0.*-manual-checklist.md` instead.
5. Type-check with `cargo clippy --target x86_64-pc-windows-msvc --all-targets --features selftest`
   (see `xb-verify`). The run itself needs Windows + network; if you could not
   run it, say so.
6. In the commit message name the new steps and the new count, e.g.
   "New steps 315-318: … EXPECTED_CHECKS 697."
