---
name: xb-settings-field
description: How to add a setting or a field to any persisted X-Browser file (settings.json, session.json, bookmarks.json, extensions.json, history.jsonl) safely. Use when adding a preference, a switch in the settings page, or any new data stored under %LOCALAPPDATA%\X-Browser.
---

# Persisted data and settings

All profile files live in `%LOCALAPPDATA%\X-Browser` (self-test:
`<XB_SELFTEST_DIR>\profile`, see `store::data_dir`). Everything read back is
**untrusted input**.

## Rules for every file (`src/store.rs`)

- JSON files carry `"v": 1` and go through the `store` helpers: tmp → flush →
  rename (`write_atomic`), a `.bak` of the previous good file on the first save
  of a run, an unreadable file moved to `.corrupt` and the `.bak` used instead.
  A file from a newer version is read but never overwritten. Do not write
  profile files with plain `fs::write`.
- On read: http/https only for URLs (≤ 8 KiB), titles cut to 512 chars, size
  and count caps (each file has its own; keep or add one for new lists),
  unknown values read as the default — never an error, never a panic.
- Writes and heavy reads stay off the UI thread where the module already
  does so (session ~1.5 s debounce, history every 2 s, bookmarks on a worker).

## Adding a setting (`src/settings.rs`)

1. Field on `Settings` with a lenient deserializer:
   `#[serde(deserialize_with = "lenient_default")]` for enums/strings,
   `lenient_on` for a default-on switch (add a `lenient_off`-style helper if a
   new switch defaults off). Add the default to `impl Default for Settings`.
2. Enum values get `from_id`/id strings used both in JSON and in IPC.
3. A `SettingsCmd` variant, its name in `SettingsCmd::NAMES` (`st-<short>`),
   a `parse` arm with bounds, and handling in `settings::on_command`; apply it
   at once (settings take effect immediately) and save.
4. Settings page: control in `ui/overlay.html`, wiring in `ui/js/settings.js`
   (`show` draws the value; changes send `st-<short>:<value>`). Turkish labels.
5. Tests in `settings.rs`: round trip of a full file, and the "hostile file"
   test (`{"engine":"evil",...}`) extended with a bad value for the new field
   reading as the default. Parse tests in `commands.rs`.
6. README: the `settings.json` row in "Data files" (key, values, default),
   plus the section describing the feature.

## Other files

Same pattern: lenient fields, caps, a hostile-input test, the README
"Data files" row. If the change adds a new file, add it to the self-test's
`prepare_profile` list in `src/selftest.rs` so every run starts clean.
