use super::*;
use crate::crx::testpkg::{TestKey, crx, stored, zip};
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

fn record(id: &str, version: &str, source: crate::extensions::Source, removed: bool) -> crate::extensions::Entry {
    crate::extensions::Entry {
        id: id.to_owned(),
        dir: format!(r"C:\x\{id}\{version}"),
        source,
        version: version.to_owned(),
        name: String::new(),
        enabled: true,
        installed: 0,
        removed,
    }
}

fn offer(id: &str, status: &str, version: &str) -> cws::Update {
    cws::Update {
        id: id.to_owned(),
        status: status.to_owned(),
        version: Some(version.to_owned()),
        codebase: Some(format!("https://example.com/{id}-{version}.crx")),
        hash_sha256: Some([7; 32]),
        size: Some(1000),
    }
}

#[test]
fn wants_only_newer_store_versions() {
    use crate::extensions::Source::{Cws, Unpacked};
    let (a, b, c, d, e) = ("a".repeat(32), "b".repeat(32), "c".repeat(32), "d".repeat(32), "e".repeat(32));
    let records = [
        record(&a, "1.0", Cws, false),
        record(&b, "2.0", Cws, false),
        record(&c, "1.0", Unpacked, false),
        record(&d, "1.0", Cws, true),
    ];
    let mut no_hash = offer(&a, "ok", "1.1");
    no_hash.hash_sha256 = None;
    let offered = [
        offer(&a, "ok", "1.1"),
        offer(&b, "ok", "2.0"),       // the same version
        offer(&b, "ok", "1.9"),       // older
        offer(&c, "ok", "9.0"),       // unpacked, not from the store
        offer(&d, "ok", "9.0"),       // removed
        offer(&e, "ok", "9.0"),       // not installed
        offer(&a, "noupdate", "9.0"), // not offered
        no_hash,
    ];
    let got = wanted(&records, &listed(&records), &offered, &[]);
    assert_eq!(got.len(), 1, "{got:?}");
    let (id, Source::Update { codebase, sha256, version, size }) = &got[0] else { panic!("{got:?}") };
    assert_eq!((id.as_str(), version.as_str(), sha256, *size), (a.as_str(), "1.1", &[7; 32], Some(1000)));
    assert_eq!(codebase, &format!("https://example.com/{a}-1.1.crx"));
    // A record without a folder or a valid version is never checked.
    let mut bad = record(&a, "1.0", Cws, false);
    bad.dir.clear();
    let mut odd = record(&b, "1.0", Cws, false);
    odd.version = "x".into();
    let both = [bad, odd];
    assert!(store_installed(&both, &listed(&both)).is_empty());
}

fn listed(records: &[crate::extensions::Entry]) -> Vec<crate::extensions::Listed> {
    records
        .iter()
        .map(|e| crate::extensions::Listed {
            id: e.id.clone(),
            name: e.name.clone(),
            version: e.version.clone(),
            enabled: e.enabled,
            source: Some(e.source),
            installed: true,
        })
        .collect()
}

#[test]
fn leaves_extensions_turned_off_alone() {
    use crate::extensions::Source::Cws;
    let (a, b) = ("a".repeat(32), "b".repeat(32));
    let mut off = record(&a, "1.0", Cws, false);
    off.enabled = false;
    let records = [off, record(&b, "1.0", Cws, false)];
    let offered = [offer(&a, "ok", "1.1"), offer(&b, "ok", "1.1")];
    let got: Vec<String> = wanted(&records, &listed(&records), &offered, &[]).into_iter().map(|(id, _)| id).collect();
    assert_eq!(got, std::slice::from_ref(&b));
    // Off in WebView2 though on in the record (turned off since): left alone too.
    let mut live = listed(&records);
    live[1].enabled = false;
    assert!(wanted(&records, &live, &offered, &[]).is_empty());
    // Not listed at all: left alone.
    assert!(store_installed(&records, &[]).is_empty());
}

#[test]
fn a_failed_version_is_not_fetched_again() {
    use crate::extensions::Source::Cws;
    let a = "a".repeat(32);
    let records = [record(&a, "1.0", Cws, false)];
    let failed = [(a.clone(), "1.1.0".to_owned())];
    // "1.1" is the version that failed ("1.1.0"); a newer one is fetched.
    assert!(wanted(&records, &listed(&records), &[offer(&a, "ok", "1.1")], &failed).is_empty());
    assert_eq!(wanted(&records, &listed(&records), &[offer(&a, "ok", "1.2")], &failed).len(), 1);
    let mut state = State::default();
    state.remember_failed(&a, "1.1");
    state.remember_failed(&a, "1.2");
    assert_eq!(state.failed, [(a.clone(), "1.2".to_owned())], "one per id");
}

#[test]
fn update_checks_are_daily() {
    let day = UPDATE_CHECK_EVERY;
    assert!(update_check_due(None, 0));
    assert!(update_check_due(None, 1_000_000));
    let last = 1_700_000_000;
    assert!(!update_check_due(Some(last), last));
    assert!(!update_check_due(Some(last), last + 23 * 3600));
    assert!(!update_check_due(Some(last), last + day - 1));
    assert!(update_check_due(Some(last), last + day));
    assert!(update_check_due(Some(last), last + 30 * day));
    // The clock went back: due now, and not again once that time is kept.
    let back = last - 3600;
    assert!(update_check_due(Some(last), back));
    assert!(!update_check_due(Some(back), back + 60));
    assert!(update_check_due(Some(0), u64::MAX));
}

#[test]
fn old_versions_are_retired_only_from_their_own_folder() {
    let root = Path::new(r"C:\data\extensions");
    let id = "a".repeat(32);
    let home = root.join(&id);
    let new = home.join("2.0");
    assert!(retirable(root, &id, &home.join("1.0"), &new));
    assert!(retirable(root, &id, &home.join("1.0_2"), &new));
    assert!(retirable(root, &id, &home.join(format!("1.0_{MAX_VERSION_FOLDERS}")), &new));
    // Not a version folder's name.
    for name in ["..", ".", "x", "1.0.", "1.0_1", "1.0_02", "1.0_99", "1.0_", "_2", ".1.0.removed-1"] {
        assert!(!retirable(root, &id, &home.join(name), &new), "{name}");
    }
    assert!(!retirable(root, &id, &home.join("..").join("1.0"), &new));
    // A "." part names the same folder (`Path::components` drops it).
    assert!(retirable(root, &id, &home.join(".").join("1.0"), &new));
    // Another id's folder, a folder deeper down, the id folder itself.
    assert!(!retirable(root, &id, &root.join("b".repeat(32)).join("1.0"), &new));
    assert!(!retirable(root, &id, &home.join("1.0").join("1.0"), &new));
    assert!(!retirable(root, &id, &home, &new));
    assert!(!retirable(root, &id, root, &new));
    // The folder now in use, however it is written.
    assert!(!retirable(root, &id, &new, &new));
    assert!(!retirable(root, &id, Path::new(&format!("{}\\", new.display())), &new));
}

/// An update source for `bytes` as offered at `version`.
fn update_for(bytes: &[u8], version: &str) -> Source {
    Source::Update {
        codebase: "https://example.com/x.crx".into(),
        sha256: crx::sha256(&[bytes]).unwrap(),
        version: version.into(),
        size: Some(bytes.len()),
    }
}

#[test]
fn verifies_an_update() {
    let key = TestKey::rsa();
    let id = key.id();
    let old = manifest("A", "1.0", r#","permissions":["storage"]"#);
    let same = package(&key, &[("manifest.json", &manifest("A", "1.1", r#","permissions":["storage"]"#))]);
    let wider = manifest("A", "1.1", r#","permissions":["storage","history"],"host_permissions":["https://b.example/*"]"#);
    let wide = package(&key, &[("manifest.json", &wider)]);

    // The same permissions: not widened.
    let f = verify_update(same.clone(), &id, &update_for(&same, "1.1"), Some(old.as_bytes()), Policy::SelfSigned).unwrap();
    assert!(!f.widened);
    assert_eq!(f.info.version, "1.1");
    // "1.1" offered as "1.1.0": the same version.
    assert!(verify_update(same.clone(), &id, &update_for(&same, "1.1.0"), Some(old.as_bytes()), Policy::SelfSigned).is_ok());

    // Widened: only the new permissions are listed.
    let f = verify_update(wide.clone(), &id, &update_for(&wide, "1.1"), Some(old.as_bytes()), Policy::SelfSigned).unwrap();
    assert!(f.widened);
    assert_eq!(f.info.permissions, cws::added_permissions(old.as_bytes(), wider.as_bytes()));
    assert_eq!(f.info.permissions.len(), 2, "{:?}", f.info.permissions);
    assert!(!f.info.permissions.iter().any(|l| cws::describe_permissions(br#"{"permissions":["storage"]}"#).contains(l)));
    // The installed manifest unreadable: widened, and every permission listed.
    let f = verify_update(same.clone(), &id, &update_for(&same, "1.1"), None, Policy::SelfSigned).unwrap();
    assert!(f.widened);
    assert_eq!(f.info.permissions, cws::describe_permissions(manifest("A", "1.1", r#","permissions":["storage"]"#).as_bytes()));

    // Another hash.
    let mut wrong = update_for(&same, "1.1");
    if let Source::Update { sha256, .. } = &mut wrong {
        sha256[0] ^= 1;
    }
    let e = verify_update(same.clone(), &id, &wrong, Some(old.as_bytes()), Policy::SelfSigned).unwrap_err();
    assert_eq!(e, Failure::Package("özet uyuşmuyor".into()));
    // Not the version offered: another one, or the installed one back.
    for v in ["1.2", "1.0", "1.1.1"] {
        let e = verify_update(same.clone(), &id, &update_for(&same, v), Some(old.as_bytes()), Policy::SelfSigned).unwrap_err();
        assert_eq!(e, Failure::Package("sürüm uyuşmuyor".into()), "{v}");
    }
    // Larger than the check said.
    let mut small = update_for(&same, "1.1");
    if let Source::Update { size, .. } = &mut small {
        *size = Some(same.len() - 1);
    }
    let e = verify_update(same.clone(), &id, &small, Some(old.as_bytes()), Policy::SelfSigned).unwrap_err();
    assert_eq!(e, Failure::Package("boyut uyuşmuyor".into()));
    // Another id's package, with the right hash.
    let other = TestKey::rsa();
    assert!(matches!(
        verify_update(same.clone(), &other.id(), &update_for(&same, "1.1"), Some(old.as_bytes()), Policy::SelfSigned),
        Err(Failure::Package(_))
    ));
    // From the store: a package of the id, never widened.
    let f = verify_update(wide.clone(), &id, &Source::Store, Some(old.as_bytes()), Policy::SelfSigned).unwrap();
    assert!(!f.widened);
}

#[test]
fn update_downloads_are_capped() {
    assert_eq!(body_cap(None), crx::MAX_CRX_BYTES);
    assert_eq!(body_cap(Some(1234)), 1234);
    assert_eq!(body_cap(Some(usize::MAX)), crx::MAX_CRX_BYTES);
}

#[test]
fn reads_the_installed_manifest_within_its_cap() {
    let dir = temp_dir("webstore-old-manifest");
    assert_eq!(read_manifest_bytes(&dir), None);
    std::fs::write(dir.join("manifest.json"), b"{}").unwrap();
    assert_eq!(read_manifest_bytes(&dir).as_deref(), Some(&b"{}"[..]));
    let big = vec![b' '; crate::extensions::MAX_MANIFEST_BYTES as usize + 1];
    std::fs::write(dir.join("manifest.json"), big).unwrap();
    assert_eq!(read_manifest_bytes(&dir), None);
    std::fs::remove_file(dir.join("manifest.json")).unwrap();
    std::fs::create_dir(dir.join("manifest.json")).unwrap();
    assert_eq!(read_manifest_bytes(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The install flow without an `App`: download, check, unpack, for a first
/// install and then an update, through the pieces the worker threads run.
#[test]
fn installs_and_updates_through_the_pure_pieces() {
    let root = temp_dir("webstore-flow").join("ext");
    let key = TestKey::rsa();
    let id = key.id();
    let perms = r#","permissions":["storage"]"#;

    // Download: the body as `fetch` gets it from `http_get`.
    let first = package(&key, &[("manifest.json", &manifest("A", "1.0", perms)), ("a.js", "1")]);
    // Check: a store install reads the package, nothing is compared.
    let f = verify_update(first, &id, &Source::Store, None, Policy::SelfSigned).unwrap();
    assert!(!f.widened);
    assert_eq!((f.info.name.as_str(), f.info.version.as_str()), ("A", "1.0"));
    // Unpack: into a fresh version folder.
    let u = unpack_to(&root, &f.bytes, &id, &f.info.version, None, Policy::SelfSigned).unwrap();
    assert!(u.fresh);
    assert_eq!(u.dir, root.join(&id).join("1.0"));
    assert_eq!(std::fs::read_to_string(u.dir.join("a.js")).unwrap(), "1");

    // The update compares with the manifest just unpacked.
    let installed = read_manifest_bytes(&u.dir).unwrap();
    let second = package(&key, &[("manifest.json", &manifest("A", "1.1", perms)), ("a.js", "2")]);
    let offer = update_for(&second, "1.1");
    let f = verify_update(second.clone(), &id, &offer, Some(&installed), Policy::SelfSigned).unwrap();
    assert!(!f.widened, "the same permissions do not ask");
    let u2 = unpack_to(&root, &f.bytes, &id, &f.info.version, Some(&u.dir), Policy::SelfSigned).unwrap();
    assert_eq!(u2.dir, root.join(&id).join("1.1"));
    assert_eq!(std::fs::read_to_string(u2.dir.join("a.js")).unwrap(), "2");
    // The old version may go once the new one is in; the new one may not.
    assert!(retirable(&root, &id, &u.dir, &u2.dir));
    assert!(!retirable(&root, &id, &u2.dir, &u2.dir));

    // A download that is not the one offered stops at the check: no folder.
    let other = package(&key, &[("manifest.json", &manifest("A", "1.2", perms))]);
    let e = verify_update(other, &id, &update_for(&second, "1.2"), Some(&installed), Policy::SelfSigned).unwrap_err();
    assert_eq!(e, Failure::Package("özet uyuşmuyor".into()));
    assert!(!root.join(&id).join("1.2").exists());

    // A download whose job has ended stops before the network (or the
    // self-test's fake store) is asked.
    let cancel = Arc::new(AtomicBool::new(true));
    let r = fetch(&id, "157.0.4341.46", &Source::Store, None, Policy::SelfSigned, cancel.clone());
    assert!(matches!(&r, Err(Failure::Net(e)) if e == "cancelled"), "{r:?}");
    let r = fetch(&id, "157.0.4341.46", &offer, Some(&u.dir), Policy::SelfSigned, cancel);
    assert!(matches!(&r, Err(Failure::Net(e)) if e == "cancelled"), "{r:?}");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn keeps_the_last_unseen_error_leniently() {
    use crate::extensions::{ExtFile, Extensions};
    let id = TestKey::rsa().id();

    // Cleaned: no control characters, trimmed, capped; nothing for a bad id
    // or no text.
    let e = LastError::new(&id, "  Uzantı indirilemedi\n(x).\u{7} ", 5).unwrap();
    assert_eq!((e.id.as_str(), e.message.as_str(), e.at), (id.as_str(), "Uzantı indirilemedi(x).", 5));
    assert_eq!(LastError::new(&id, &"ş".repeat(1000), 1).unwrap().message.chars().count(), MAX_ERROR_TEXT);
    assert_eq!(LastError::new("not-an-id", "x", 1), None);
    assert_eq!(LastError::new(&id, " \n\u{7}", 1), None);

    // Written only when there is one, and read back as written.
    assert!(!serde_json::to_string(&ExtFile::default()).unwrap().contains("last_error"));
    let file = ExtFile { last_error: Some(e.clone()), ..ExtFile::default() };
    let back: ExtFile = serde_json::from_str(&serde_json::to_string(&file).unwrap()).unwrap();
    assert_eq!(back.last_error, Some(e.clone()));

    // Anything else is no record, and costs nothing else in the file.
    let bad = [
        "null".to_string(),
        r#""x""#.into(),
        "1".into(),
        "[]".into(),
        r#"{"id":"abc","message":"m","at":1}"#.into(),
        format!(r#"{{"id":"{id}","message":5,"at":1}}"#),
        format!(r#"{{"id":"{id}","message":"m","at":-1}}"#),
        format!(r#"{{"id":"{id}","message":"m","at":"1"}}"#),
        format!(r#"{{"id":"{id}","message":"m"}}"#),
        format!(r#"{{"id":"{id}","message":"\u0007","at":1}}"#),
    ];
    for b in bad {
        let text = format!(r#"{{"items":[],"last_update_check":7,"last_error":{b}}}"#);
        let f: ExtFile = serde_json::from_str(&text).unwrap_or_else(|err| panic!("{b}: {err}"));
        assert_eq!((f.last_error, f.last_update_check), (None, Some(7)), "{b}");
    }
    // Text read back is cleaned as when written.
    let raw = format!(r#"{{"last_error":{{"id":"{id}","message":"a\nb","at":3}}}}"#);
    let f: ExtFile = serde_json::from_str(&raw).unwrap();
    assert_eq!(f.last_error.map(|e| e.message), Some("ab".to_string()));

    // One record: a later error takes the place of the one before, and the
    // next start reads it.
    let dir = temp_dir("webstore-last-error");
    let mut x = Extensions::open(Some(&dir));
    assert_eq!(x.last_error(), None);
    x.set_last_error(e);
    let later = LastError::new(&id, "Uzantı eklenemedi (y).", 9).unwrap();
    x.set_last_error(later.clone());
    drop(x);
    assert!(crate::store::flush(Duration::from_secs(5)));
    let x = Extensions::open(Some(&dir));
    assert_eq!(x.last_error(), Some(&later));
    drop(x);
    let _ = std::fs::remove_dir_all(&dir);
}
