//! Embeds assets/taser.ico in the exe as icon resource 1, with no
//! build dependencies: the .ico is rewritten as a compiled resource file
//! (.res) and handed straight to link.exe, which accepts .res inputs. That
//! gives Explorer its file icon, and main.rs loads the same resource for the
//! window and the taskbar. Regenerate the .ico with assets/make_icon.py.

use std::env;
use std::fs;
use std::path::PathBuf;

const ICON: &str = "assets/taser.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || target_env != "msvc" {
        return;
    }
    let ico = fs::read(ICON).unwrap_or_else(|e| panic!("{ICON}: {e}"));
    let res = to_res(&ico).unwrap_or_else(|e| panic!("{ICON}: {e}"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("taser.res");
    fs::write(&out, res).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// ICO file → .res: one RT_ICON per image (ids 1..n) and an RT_GROUP_ICON
/// (id 1) whose directory points at them by id instead of by file offset.
fn to_res(ico: &[u8]) -> Result<Vec<u8>, String> {
    if u16_at(ico, 0) != Some(0) || u16_at(ico, 2) != Some(1) {
        return Err("not an icon file".into());
    }
    let count = u16_at(ico, 4).ok_or("truncated")?;
    if count == 0 {
        return Err("no images".into());
    }
    let mut res = Vec::new();
    entry(&mut res, 0, 0, 0, &[]); // the empty entry every .res starts with
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&count.to_le_bytes());
    for i in 0..count {
        let at = 6 + 16 * usize::from(i);
        let dir = ico.get(at..at + 16).ok_or("truncated directory")?;
        let size = u32_at(dir, 8).ok_or("truncated")? as usize;
        let offset = u32_at(dir, 12).ok_or("truncated")? as usize;
        let image = ico
            .get(offset..offset.checked_add(size).ok_or("bad size")?)
            .ok_or("image past end of file")?;
        let id = i + 1;
        entry(&mut res, RT_ICON, id, 0x1010, image);
        group.extend_from_slice(&dir[..12]); // w, h, colors, reserved, planes, bpp, bytes
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    Ok(res)
}

/// One RESOURCEHEADER (numeric type and name) plus its data, DWORD-aligned.
fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    let ord = |v: u16| [0xFF, 0xFF, v as u8, (v >> 8) as u8];
    res.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    res.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    res.extend_from_slice(&ord(kind));
    res.extend_from_slice(&ord(name));
    res.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    res.extend_from_slice(&flags.to_le_bytes()); // MemoryFlags
    let lang = if kind == 0 { 0 } else { LANG_EN_US };
    res.extend_from_slice(&lang.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // Version
    res.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(data);
    while !res.len().is_multiple_of(4) {
        res.push(0);
    }
}
