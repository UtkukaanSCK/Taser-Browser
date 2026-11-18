use super::*;
use crate::crx::tests::{TestKey, crx, stored, zip};
use crate::store::tests::temp_dir;

fn package(key: &TestKey, files: &[(&str, &str)]) -> Vec<u8> {
    let ents: Vec<_> = files.iter().map(|(n, d)| stored(n, d.as_bytes())).collect();
    crx(&zip(&ents), &[key])
}

fn manifest(name: &str, version: &str, extra: &str) -> String {
    format!(r#"{{"manifest_version":3,"name":{},"version":"{version}"{extra}}}"#, serde_json::to_string(name).unwrap())
}

#[test]
fn commands_parse_and_answers_are_the_overlays() {
    for n in StoreCmd::NAMES {
        let c = StoreCmd::parse(n).unwrap();
        assert_eq!(c.is_answer(), n != "ext-store");
    }
    assert_eq!(StoreCmd::parse("ext-yes "), None);
    assert_eq!(StoreCmd::parse("EXT-YES"), None);
}

#[test]
fn every_failure_has_text() {
    for f in [
        Failure::Cancelled,
        Failure::Busy,
        Failure::BadId,
        Failure::NoView,
        Failure::Net("x".into()),
        Failure::Package("x".into()),
        Failure::Install("x".into()),
    ] {
        assert!(!f.message().is_empty(), "{f:?}");
    }
}

#[test]
fn locales() {
    for ok in ["en", "tr", "pt_BR", "zh_CN"] {
        assert!(valid_locale(ok), "{ok}");
    }
    for bad in ["", "e", "../en", "en/..", "en\\x", "a.b", "abcdefghijk", "tr-TR"] {
        assert!(!valid_locale(bad), "{bad}");
    }
}

#[test]
fn names_are_looked_up_in_locales() {
    let files = |path: &str| -> Option<Vec<u8>> {
        match path {
            "_locales/de/messages.json" => Some(r#"{"AppName":{"message":"Deutsch"}}"#.into()),
            "_locales/en/messages.json" => Some("\u{feff}{\"appname\":{\"message\":\"English\"},\"Empty\":{\"message\":\" \"}}".into()),
            _ => None,
        }
    };
    assert_eq!(resolve_name("Plain", None, files), "Plain");
    // Keys match without regard to case; the default locale comes before en.
    assert_eq!(resolve_name("__MSG_appName__", Some("de"), files), "Deutsch");
    assert_eq!(resolve_name("__MSG_appName__", None, files), "English");
    // A bad default locale is never read as a path.
    assert_eq!(resolve_name("__MSG_appName__", Some("../de"), files), "English");
    // Missing or empty: the raw name stays.
    assert_eq!(resolve_name("__MSG_Empty__", None, files), "__MSG_Empty__");
    assert_eq!(resolve_name("__MSG_nope__", None, files), "__MSG_nope__");
    let tr = |path: &str| (path == "_locales/tr/messages.json").then(|| r#"{"n":{"message":"Türkçe"}}"#.as_bytes().to_vec());
    assert_eq!(resolve_name("__MSG_n__", Some("en"), tr), "Türkçe");
    assert_eq!(resolve_name("__MSG_n__", None, |_| Some(b"not json".to_vec())), "__MSG_n__");
}

#[test]
fn names_cannot_hide_or_flood() {
    assert_eq!(clean_name("  A \n\t  B  "), "A B");
    assert_eq!(clean_name("Good\u{202E}lpp\u{200B}a\u{E0041}"), "Goodlppa");
    assert_eq!(clean_name("\u{0}\u{7}x"), "x");
    let long = "ş".repeat(200);
    let c = clean_name(&long);
    assert_eq!(c.chars().count(), MAX_NAME_CHARS);
    assert!(c.ends_with('…'));
    assert_eq!(clean_name(&"a".repeat(MAX_NAME_CHARS)), "a".repeat(MAX_NAME_CHARS));
}

#[test]
fn inspects_a_package() {
    let key = TestKey::rsa();
    let id = key.id();
    let m = manifest("__MSG_name__", "1.2.3", r#","default_locale":"en","permissions":["tabs"],"host_permissions":["<all_urls>"]"#);
    // A right-to-left override (U+202E) inside the name, written raw in the
    // JSON, never reaches the question.
    let messages = format!(r#"{{"name":{{"message":"Örnek{} uzantı"}}}}"#, '\u{202E}');
    let bytes = package(&key, &[("manifest.json", &m), ("_locales/en/messages.json", &messages)]);
    let info = inspect(&bytes, &id, Policy::SelfSigned).unwrap();
    assert_eq!(info.name, "Örnek uzantı");
    assert_eq!(info.version, "1.2.3");
    assert_eq!(info.permissions, cws::describe_permissions(m.as_bytes()));
    assert!(!info.permissions.is_empty());

    // Another id, a bad version, no manifest, a manifest that is no object.
    let other = TestKey::rsa();
    assert!(matches!(inspect(&bytes, &other.id(), Policy::SelfSigned), Err(Failure::Package(_))));
    let bad = package(&key, &[("manifest.json", &manifest("x", "1.0.0.0.0", ""))]);
    assert!(matches!(inspect(&bad, &id, Policy::SelfSigned), Err(Failure::Package(_))));
    let none = package(&key, &[("a.txt", "x")]);
    assert!(matches!(inspect(&none, &id, Policy::SelfSigned), Err(Failure::Package(_))));
    let list = package(&key, &[("manifest.json", "[1]")]);
    assert!(matches!(inspect(&list, &id, Policy::SelfSigned), Err(Failure::Package(_))));
    // The real policy wants the store's publisher proof.
    assert!(matches!(inspect(&bytes, &id, Policy::WebStore), Err(Failure::Package(_))));
}

#[test]
fn unpacks_into_a_version_folder() {
    let root = temp_dir("webstore-unpack").join("ext");
    let key = TestKey::rsa();
    let id = key.id();
    let bytes = package(&key, &[("manifest.json", &manifest("A", "2.0", "")), ("js/a.js", "1")]);

    let u = unpack_to(&root, &bytes, &id, "2.0", None, Policy::SelfSigned).unwrap();
    assert!(u.fresh);
    assert_eq!(u.dir, root.join(&id).join("2.0"));
    assert_eq!(std::fs::read_to_string(u.dir.join("js/a.js")).unwrap(), "1");

    // The folder recorded now is used as it is, never rewritten.
    std::fs::write(u.dir.join("mark"), "kept").unwrap();
    let again = unpack_to(&root, &bytes, &id, "2.0", Some(&u.dir), Policy::SelfSigned).unwrap();
    assert!(!again.fresh);
    assert_eq!(std::fs::read_to_string(again.dir.join("mark")).unwrap(), "kept");

    // Recorded but broken: refused, and nothing in it is touched.
    std::fs::remove_file(u.dir.join("manifest.json")).unwrap();
    assert!(matches!(unpack_to(&root, &bytes, &id, "2.0", Some(&u.dir), Policy::SelfSigned), Err(Failure::Install(_))));
    assert!(u.dir.join("mark").is_file());

    // A folder of this version nobody records is never deleted: it may be
    // what WebView2 loads. The package goes next to it.
    let fresh = unpack_to(&root, &bytes, &id, "2.0", None, Policy::SelfSigned).unwrap();
    assert!(fresh.fresh);
    assert_eq!(fresh.dir, root.join(&id).join("2.0_2"));
    assert!(u.dir.join("mark").is_file() && fresh.dir.join("js/a.js").is_file());

    // A new version goes next to the recorded one, which stays.
    let v3 = package(&key, &[("manifest.json", &manifest("A", "3.0", ""))]);
    let u3 = unpack_to(&root, &v3, &id, "3.0", Some(&fresh.dir), Policy::SelfSigned).unwrap();
    assert_eq!(u3.dir, root.join(&id).join("3.0"));
    assert!(fresh.dir.join("js/a.js").is_file());

    // Not a version, or not a package of this id: nothing is written.
    assert!(matches!(unpack_to(&root, &bytes, &id, "../x", None, Policy::SelfSigned), Err(Failure::Package(_))));
    assert!(matches!(unpack_to(&root, &bytes, &TestKey::rsa().id(), "2.0", None, Policy::SelfSigned), Err(Failure::Package(_))));

    // A file where the version folder goes is not removed either; with every
    // name taken, nothing is written.
    std::fs::write(root.join(&id).join("4.0"), "file").unwrap();
    for n in 2..=MAX_VERSION_FOLDERS {
        std::fs::write(root.join(&id).join(format!("4.0_{n}")), "file").unwrap();
    }
    assert!(matches!(unpack_to(&root, &bytes, &id, "4.0", None, Policy::SelfSigned), Err(Failure::Install(_))));
    assert_eq!(std::fs::read_to_string(root.join(&id).join("4.0")).unwrap(), "file");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}
