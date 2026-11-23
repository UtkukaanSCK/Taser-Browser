---
name: xb-commit-and-docs
description: X-Browser's commit message style and which documentation must move with a change (README sections, Data files and Layout tables, known limits, manual checklists, design notes, media). Use when writing a commit message, preparing a PR, or finishing any feature or behaviour change.
---

# Commits and docs

## Commit messages

Match the history (`git log`):

- Subject: imperative, plain English, says the user-visible behaviour, no
  prefix tags, no trailing period. Examples:
  "Pick a site's permissions with buttons, not a dropdown",
  "Add HTTPS-only, tracking-parameter stripping and Global Privacy Control".
  Turkish UI words appear as they are on screen ("Uzantılar", "Güncelle").
- Body, wrapped at ~72: what was wrong or missing, why it happened, what it
  does now. Mention trade-offs and what moved in tests (e.g. "Popups are
  placed from UI_HEIGHT (now 76), so their unit tests move down by 36").
- Self-test changes: "New steps N-M: … EXPECTED_CHECKS <n>."
- End with the attribution trailers the session asks for.
- Never put model names in commits, code or docs.

## Docs that move with code

The README is the spec; keep it exact and in its existing voice (short
declarative sentences, Turkish UI labels in quotes, no marketing).

| Change | Update |
| --- | --- |
| New/changed shortcut | "Keyboard shortcuts" table |
| New file or field under the profile | "Data files" table (limits, when written) |
| New `src/` or `ui/` file | "Layout" table, one line, same style |
| Behaviour with a known gap | the section's "Known limits, left as they are" list |
| Something a script cannot verify | `docs/v0.3-manual-checklist.md` (or the current version's) |
| Privacy/security behaviour | "Privacy" / "Security indicator…" sections |
| Binary size or start-up | "Startup timing" table only with real measurements (Windows) |
| Icon or README pictures | edit `assets/make_icon.py` / `docs/media/make_media.py` and regenerate; never hand-edit the SVG/ICO |

Design notes (`docs/v0.*-design.md`) record research and decisions for a
version; add to them only for a new version or an explicit design question.
Never invent measurements or claim manual checks were done.
