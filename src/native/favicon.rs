//! Tab favicons. WebView2 fetches and decodes the page's icon and hands it
//! back re-encoded as PNG; only that PNG, checked and size-capped, reaches
//! the UI, as a `data:` URL it may only use as an `<img src>`. The icon's
//! own URL (FaviconUri) never leaves this module: the UI would have to load
//! it, from the network, in the profile it shares with the pages.

use super::Proxy;
use crate::tabs::TabId;
use wry::WebView;

/// Largest icon accepted (the PNG WebView2 re-encodes, before base64).
pub const MAX_ICON_BYTES: usize = 32 * 1024;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Largest icon side accepted, in pixels. A tiny PNG can still declare a
/// huge image (it compresses well), and the UI would decode it in full.
pub const MAX_ICON_SIDE: u32 = 256;

/// Signature, then the IHDR chunk: length, type, data (13 bytes), CRC.
const PNG_HEADER_LEN: usize = 8 + 4 + 4 + 13 + 4;

/// Width and height from the IHDR chunk, which must come first.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < PNG_HEADER_LEN || !bytes.starts_with(&PNG_MAGIC) || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Some((be(16), be(20)))
}

/// `data:image/png;base64,...` for a PNG of at most `MAX_ICON_BYTES` and at
/// most `MAX_ICON_SIDE` pixels a side, else None. The UI accepts nothing but
/// this exact shape (see strip.js).
pub fn png_data_url(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_ICON_BYTES {
        return None;
    }
    let (w, h) = png_size(bytes)?;
    if !(1..=MAX_ICON_SIDE).contains(&w) || !(1..=MAX_ICON_SIDE).contains(&h) {
        return None;
    }
    const PREFIX: &str = "data:image/png;base64,";
    let mut s = String::with_capacity(PREFIX.len() + bytes.len().div_ceil(3) * 4);
    s.push_str(PREFIX);
    push_base64(&mut s, bytes);
    Some(s)
}

/// Standard base64 with padding (RFC 4648 section 4).
fn push_base64(out: &mut String, bytes: &[u8]) {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::app::{PageEvent, UserEvent};
    use crate::url::is_web_url;
    use std::cell::Cell;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::*;
    use windows::Win32::System::Com::{IStream, STREAM_SEEK_SET};
    use windows::core::{BOOL, Interface, PWSTR};
    use wry::WebViewExtWindows;

    /// Reads the whole stream, or None if it is longer than `MAX_ICON_BYTES`
    /// (at most one byte more is ever read) or reading fails.
    fn read_capped(stream: &IStream) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; MAX_ICON_BYTES + 1];
        let mut total = 0usize;
        unsafe {
            let _ = stream.Seek(0, STREAM_SEEK_SET, None);
            loop {
                let mut n = 0u32;
                let rest = &mut buf[total..];
                stream.Read(rest.as_mut_ptr().cast(), rest.len() as u32, Some(&mut n)).ok().ok()?;
                if n == 0 {
                    break;
                }
                // A stream claiming more than it was given room for is broken.
                if n as usize > rest.len() {
                    return None;
                }
                total += n as usize;
                if total >= buf.len() {
                    return None;
                }
            }
        }
        buf.truncate(total);
        Some(buf)
    }

    /// Whether the page in `wv` has an icon of its own: it is a web page and
    /// WebView2 has an icon URL for it (FaviconUri is empty for about:blank
    /// and for pages that link no icon and have no `/favicon.ico`). The URL
    /// is only tested for emptiness, never passed on.
    fn has_own_icon(wv: &ICoreWebView2_15) -> bool {
        let read = |get: &dyn Fn(&mut PWSTR) -> windows::core::Result<()>| {
            let mut s = PWSTR::null();
            get(&mut s).map(|_| take_pwstr(s)).unwrap_or_default()
        };
        let source = read(&|s| unsafe { wv.Source(s) });
        let icon = read(&|s| unsafe { wv.FaviconUri(s) });
        is_web_url(&source) && !icon.is_empty()
    }

    /// Asks WebView2 for the current icon. Only the newest request's answer
    /// counts (`latest`), so a slow answer for the previous page can never
    /// overwrite the next page's icon.
    ///
    /// GetFavicon never says "none": for a page without an icon (about:blank,
    /// a site with no `/favicon.ico`) it answers with WebView2's generic
    /// placeholder, a valid PNG. That is not the page's icon, so a page
    /// without one of its own gets None and GetFavicon is not asked at all.
    fn request(wv: &ICoreWebView2, tab: TabId, proxy: &Proxy, latest: &Rc<Cell<u64>>) {
        let Ok(wv) = wv.cast::<ICoreWebView2_15>() else { return };
        let me = latest.get() + 1;
        latest.set(me);
        if !has_own_icon(&wv) {
            let _ = proxy.send_event(UserEvent::Page(tab, PageEvent::Favicon(None)));
            return;
        }
        let (p, latest) = (proxy.clone(), latest.clone());
        let done = GetFaviconCompletedHandler::create(Box::new(move |hr, stream| {
            if latest.get() != me {
                return Ok(());
            }
            let icon = match (hr, stream) {
                (Ok(()), Some(s)) => read_capped(&s).and_then(|b| png_data_url(&b)),
                _ => None,
            };
            let _ = p.send_event(UserEvent::Page(tab, PageEvent::Favicon(icon)));
            Ok(())
        }));
        unsafe {
            let _ = wv.GetFavicon(COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, &done);
        }
    }

    /// Keeps the tab's icon current: fetched when the page's icon changes and
    /// when a load completes (a new page with the same icon URL raises no
    /// FaviconChanged), dropped as soon as a new document starts and whenever
    /// the page has no icon of its own (see `request`).
    /// Runtimes without ICoreWebView2_15 simply show no icons.
    pub fn attach_favicon(view: &WebView, tab: TabId, proxy: &Proxy) -> windows::core::Result<()> {
        let wv = view.webview();
        let Ok(wv15) = wv.cast::<ICoreWebView2_15>() else { return Ok(()) };
        let latest = Rc::new(Cell::new(0u64));
        let mut token = 0i64;
        unsafe {
            let (p, l) = (proxy.clone(), latest.clone());
            wv15.add_FaviconChanged(
                &FaviconChangedEventHandler::create(Box::new(move |wv, _| {
                    if let Some(wv) = wv {
                        request(&wv, tab, &p, &l);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let (p, l) = (proxy.clone(), latest.clone());
            wv.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |wv, _| {
                    if let Some(wv) = wv {
                        request(&wv, tab, &p, &l);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let (p, l) = (proxy.clone(), latest);
            wv.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |_, args| {
                    let mut new_doc = BOOL(0);
                    if let Some(args) = args {
                        args.IsNewDocument(&mut new_doc)?;
                    }
                    if new_doc.as_bool() {
                        // Any answer still in flight belongs to the old page.
                        l.set(l.get() + 1);
                        let _ = p.send_event(UserEvent::Page(tab, PageEvent::Favicon(None)));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn attach_favicon(_: &WebView, _: TabId, _: &Proxy) -> Result<(), ()> {
        Ok(())
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG signature and IHDR for a `w` x `h` image, padded to `len` bytes.
    fn png_sized(len: usize, w: u32, h: u32) -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v.extend_from_slice(&[0; 4]); // CRC (not checked)
        assert!(len >= v.len());
        v.resize(len, 0xAB);
        v
    }

    fn png(len: usize) -> Vec<u8> {
        png_sized(len, 16, 16)
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, want) in [
            ("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy"),
        ] {
            let mut s = String::new();
            push_base64(&mut s, input.as_bytes());
            assert_eq!(s, want, "{input:?}");
        }
        let mut s = String::new();
        push_base64(&mut s, &[0xFB, 0xFF, 0xBF]);
        assert_eq!(s, "+/+/", "both high symbols");
    }

    #[test]
    fn accepts_only_small_pngs() {
        let ok = png_data_url(&png(PNG_HEADER_LEN)).expect("smallest accepted");
        assert!(ok.starts_with("data:image/png;base64,iVBORw0KGg"), "{ok}");
        assert!(png_data_url(&png(MAX_ICON_BYTES)).is_some(), "exactly at the cap");
        assert_eq!(png_data_url(&png(MAX_ICON_BYTES + 1)), None, "one byte over");
        assert_eq!(png_data_url(&PNG_MAGIC), None, "magic alone is no image");
        assert_eq!(png_data_url(&[]), None);
        let mut gif = b"GIF89a".to_vec();
        gif.resize(64, 0);
        assert_eq!(png_data_url(&gif), None, "not a PNG");
        let mut svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        svg.resize(64, b' ');
        assert_eq!(png_data_url(&svg), None, "never SVG (scriptable)");
        let mut bad = png(64);
        bad[7] = 0;
        assert_eq!(png_data_url(&bad), None, "last magic byte wrong");
        assert_eq!(png_data_url(&png(64)[..PNG_HEADER_LEN - 1]), None, "IHDR cut short");
    }

    #[test]
    fn caps_pixel_dimensions() {
        assert!(png_data_url(&png_sized(64, 1, 1)).is_some(), "1x1");
        assert!(png_data_url(&png_sized(64, 256, 256)).is_some(), "exactly at the side cap");
        assert!(png_data_url(&png_sized(64, 256, 1)).is_some(), "wide but in bounds");
        assert_eq!(png_data_url(&png_sized(64, 257, 16)), None, "too wide");
        assert_eq!(png_data_url(&png_sized(64, 16, 257)), None, "too tall");
        assert_eq!(png_data_url(&png_sized(64, 100_000, 100_000)), None, "decompression bomb");
        assert_eq!(png_data_url(&png_sized(64, u32::MAX, 16)), None, "absurd width");
        assert_eq!(png_data_url(&png_sized(64, 0, 16)), None, "zero width");
        assert_eq!(png_data_url(&png_sized(64, 16, 0)), None, "zero height");
    }

    #[test]
    fn requires_ihdr_first() {
        let mut v = png(64);
        v[12..16].copy_from_slice(b"IDAT");
        assert_eq!(png_data_url(&v), None, "another chunk first");
        let mut v = png(64);
        v[12..16].copy_from_slice(b"ihdr");
        assert_eq!(png_data_url(&v), None, "case matters");
        let mut v = PNG_MAGIC.to_vec();
        v.resize(64, 0xAB);
        assert_eq!(png_data_url(&v), None, "magic and filler, no IHDR");
    }

    #[test]
    fn data_url_has_only_base64_characters() {
        let bytes: Vec<u8> = png(PNG_HEADER_LEN).into_iter().chain((0..=255u8).cycle().take(1000)).collect();
        let url = png_data_url(&bytes).unwrap();
        let body = url.strip_prefix("data:image/png;base64,").unwrap();
        assert_eq!(body.len(), bytes.len().div_ceil(3) * 4);
        assert!(body.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c)));
    }
}
