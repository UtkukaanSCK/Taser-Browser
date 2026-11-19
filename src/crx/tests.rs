//! Tests for `crx`: packages signed here with throwaway CNG keys, plus hand-built
//! hostile headers and zips. A real Web Store package is checked by the
//! ignored `real_crx` test (`XB_CRX_FIXTURE`, `XB_CRX_ID`).

use super::*;
use super::testpkg::*;
use crate::store::tests::temp_dir;

fn sample_zip() -> Vec<u8> {
    zip(&[
        deflated("manifest.json", br#"{"manifest_version":3,"name":"t","version":"1"}"#),
        dir("js/"),
        deflated("js/a.js", &b"console.log(1);\n".repeat(200)),
        stored("img/x.bin", &[0, 1, 2, 3, 255]),
    ])
}

fn zip_err(ents: &[Ent]) -> CrxError {
    read_zip(&zip(ents)).unwrap_err()
}

// --- verify_crx3 -------------------------------------------------------------

#[test]
fn verifies_rsa_package() {
    let key = TestKey::rsa();
    let z = sample_zip();
    let c = crx(&z, &[&key]);
    let p = verify_crx3(&c, &key.id(), Policy::SelfSigned).unwrap();
    assert_eq!(p.zip, &z[..]);
    assert_eq!(p.id, key.id());
    assert_eq!(p.key_hashes, vec![cng::sha256(&[&key.spki]).unwrap()]);
    assert_eq!(p.developer_key, &key.spki[..]);
}

#[test]
fn verifies_rsa_plus_ecdsa_and_ecdsa_developer_key() {
    let (rsa, ec) = (TestKey::rsa(), TestKey::p256());
    let z = sample_zip();
    let c = crx(&z, &[&rsa, &ec]);
    let p = verify_crx3(&c, &rsa.id(), Policy::SelfSigned).unwrap();
    assert_eq!(p.key_hashes.len(), 2);
    assert_eq!(p.developer_key, &rsa.spki[..]);
    // The developer key may be the ECDSA one too.
    let c = crx(&z, &[&ec, &rsa]);
    let p = verify_crx3(&c, &ec.id(), Policy::SelfSigned).unwrap();
    assert_eq!(p.developer_key, &ec.spki[..]);
}

#[test]
fn rejects_wrong_or_invalid_expected_id() {
    let (key, other) = (TestKey::rsa(), TestKey::rsa());
    let c = crx(&sample_zip(), &[&key]);
    assert!(matches!(verify_crx3(&c, &other.id(), Policy::SelfSigned), Err(CrxError::IdMismatch)));
    for bad in ["", "abc", &key.id().to_uppercase(), &format!("{}q", &key.id()[..31])] {
        assert!(matches!(verify_crx3(&c, bad, Policy::SelfSigned), Err(CrxError::BadExpectedId)), "{bad}");
    }
}

#[test]
fn rejects_id_not_backed_by_a_proof_key() {
    // Signed data claims key A's id, but only key B signed it.
    let (a, b) = (TestKey::rsa(), TestKey::rsa());
    let c = crx_signed(&sample_zip(), &a.crx_id(), &[&b]);
    assert!(matches!(verify_crx3(&c, &a.id(), Policy::SelfSigned), Err(CrxError::NoDeveloperProof)));
    // No proofs at all.
    let c = assemble(&header(&[], &signed_data(&a.crx_id())), &sample_zip());
    assert!(matches!(verify_crx3(&c, &a.id(), Policy::SelfSigned), Err(CrxError::NoDeveloperProof)));
}

#[test]
fn rejects_bad_magic_version_and_sizes() {
    let key = TestKey::rsa();
    let good = crx(&sample_zip(), &[&key]);
    let id = key.id();

    let mut c = good.clone();
    c[0] = b'P';
    assert!(matches!(verify_crx3(&c, &id, Policy::SelfSigned), Err(CrxError::BadMagic)));
    assert!(matches!(verify_crx3(b"Cr2", &id, Policy::SelfSigned), Err(CrxError::BadMagic)));
    assert!(matches!(verify_crx3(b"PK\x03\x04rest", &id, Policy::SelfSigned), Err(CrxError::BadMagic)));

    let mut c = good.clone();
    c[4] = 2;
    assert!(matches!(verify_crx3(&c, &id, Policy::SelfSigned), Err(CrxError::BadVersion(2))));

    assert!(matches!(verify_crx3(b"Cr24\x03\0\0\0\x10", &id, Policy::SelfSigned), Err(CrxError::BadHeader(_))));
    // Header length past the end of the file.
    let mut c = good[..40].to_vec();
    c[8..12].copy_from_slice(&1000u32.to_le_bytes());
    assert!(matches!(verify_crx3(&c, &id, Policy::SelfSigned), Err(CrxError::BadHeader("truncated"))));
    // Header length over the limit, and the largest u32.
    for len in [MAX_HEADER_BYTES as u32 + 1, u32::MAX] {
        let mut c = good.clone();
        c[8..12].copy_from_slice(&len.to_le_bytes());
        assert!(matches!(verify_crx3(&c, &id, Policy::SelfSigned), Err(CrxError::BadHeader("header too large"))));
    }
    // Nothing after the header.
    let header_len = le32(&good, 8) as usize;
    assert!(matches!(verify_crx3(&good[..12 + header_len], &id, Policy::SelfSigned), Err(CrxError::BadZip(_))));
    // Over the package size limit, checked before anything else is read.
    let mut big = good.clone();
    big.resize(MAX_CRX_BYTES + 1, 0);
    assert!(matches!(verify_crx3(&big, &id, Policy::SelfSigned), Err(CrxError::TooLarge)));
}

#[test]
fn rejects_tampered_zip_or_signature() {
    let key = TestKey::rsa();
    let z = sample_zip();
    let good = crx(&z, &[&key]);

    let mut c = good.clone();
    let mid = c.len() - z.len() / 2;
    c[mid] ^= 1;
    assert!(matches!(verify_crx3(&c, &key.id(), Policy::SelfSigned), Err(CrxError::BadSignature)));

    // One appended byte (the EOCD would then be misplaced; the signature
    // fails first).
    let mut c = good.clone();
    c.push(0);
    assert!(matches!(verify_crx3(&c, &key.id(), Policy::SelfSigned), Err(CrxError::BadSignature)));

    let signed = signed_data(&key.crx_id());
    let mut sig = key.sign(&digest(&signed, &z));
    sig[10] ^= 0x40;
    let c = assemble(&header(&[(2, proof(&key.spki, &sig))], &signed), &z);
    assert!(matches!(verify_crx3(&c, &key.id(), Policy::SelfSigned), Err(CrxError::BadSignature)));

    // ECDSA too.
    let ec = TestKey::p256();
    let signed = signed_data(&ec.crx_id());
    let mut sig = ec.sign(&digest(&signed, &z));
    let last = sig.len() - 1;
    sig[last] ^= 1;
    let c = assemble(&header(&[(3, proof(&ec.spki, &sig))], &signed), &z);
    assert!(matches!(verify_crx3(&c, &ec.id(), Policy::SelfSigned), Err(CrxError::BadSignature)));
}

#[test]
fn every_proof_must_hold() {
    let (dev, other) = (TestKey::rsa(), TestKey::p256());
    let z = sample_zip();
    let signed = signed_data(&dev.crx_id());
    let d = digest(&signed, &z);
    let good = proof(&dev.spki, &dev.sign(&d));
    // The second key signed something else.
    let wrong = proof(&other.spki, &other.sign(&digest(&signed, b"other zip")));
    let c = assemble(&header(&[(2, good), (3, wrong)], &signed), &z);
    assert!(matches!(verify_crx3(&c, &dev.id(), Policy::SelfSigned), Err(CrxError::BadSignature)));
}

#[test]
fn rejects_key_of_the_wrong_kind() {
    let (rsa, ec) = (TestKey::rsa(), TestKey::p256());
    let z = sample_zip();
    // An RSA proof filed under ECDSA and the other way round.
    for (key, field) in [(&rsa, 3), (&ec, 2)] {
        let signed = signed_data(&key.crx_id());
        let p = proof(&key.spki, &key.sign(&digest(&signed, &z)));
        let c = assemble(&header(&[(field, p)], &signed), &z);
        assert!(matches!(verify_crx3(&c, &key.id(), Policy::SelfSigned), Err(CrxError::BadKey)));
    }
    // Garbage where the key should be (its hash still names the id).
    let junk = b"not a key".to_vec();
    let id = id_from_hash(&cng::sha256(&[&junk]).unwrap());
    let crx_id: [u8; 16] = cng::sha256(&[&junk]).unwrap()[..16].try_into().unwrap();
    let c = assemble(&header(&[(2, proof(&junk, b"sig"))], &signed_data(&crx_id)), &z);
    assert!(matches!(verify_crx3(&c, &id, Policy::SelfSigned), Err(CrxError::BadKey)));
}

#[test]
fn rejects_malformed_protobuf() {
    let id16 = [7u8; 16];
    let signed = signed_data(&id16);
    let ok = header(&[], &signed);
    assert!(parse_header(&ok).is_ok());

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated varint", vec![0x82]),
        ("length past the end", vec![0x12, 0x05, 1, 2]),
        ("wire type 3", vec![0x13]),
        ("wire type 4", vec![0x14]),
        ("wire type 7", vec![0x17]),
        ("field number 0", vec![0x02, 0x00]),
        ("varint overflow", [vec![0x08], vec![0xff; 9], vec![0x02]].concat()),
        ("huge length", [vec![0x12], vec![0xff; 9], vec![0x01]].concat()),
        ("no signed data", vec![]),
        ("signed data repeated", [ok.clone(), ok.clone()].concat()),
        ("signed data as varint", vec![0x80, 0xe2, 0x04, 0x01]),
        ("proof as varint", [vec![0x10, 0x01], ok.clone()].concat()),
        ("proof without signature", {
            let mut p = Vec::new();
            pb_bytes(&mut p, 1, b"key");
            header(&[(2, p)], &signed)
        }),
        ("proof key repeated", {
            let mut p = proof(b"k", b"s");
            pb_bytes(&mut p, 1, b"k2");
            header(&[(2, p)], &signed)
        }),
        ("crx id 15 bytes", header(&[], &signed_data(&[1; 15]))),
        ("crx id 17 bytes", header(&[], &signed_data(&[1; 17]))),
        ("crx id repeated", header(&[], &[signed.clone(), signed.clone()].concat())),
        ("crx id missing", header(&[], &[0x10, 0x01])),
        ("too many proofs", header(&vec![(2, proof(b"k", b"s")); MAX_PROOFS + 1], &signed)),
    ];
    for (what, bytes) in cases {
        assert!(matches!(parse_header(&bytes), Err(CrxError::BadHeader(_))), "{what}");
    }

    // Unknown fields (all wire types we accept) are skipped.
    let mut h = vec![0x08, 0x96, 0x01, 0x29, 0, 0, 0, 0, 0, 0, 0, 0, 0x35, 0, 0, 0, 0];
    pb_bytes(&mut h, 99, b"whatever");
    h.extend(&ok);
    assert_eq!(parse_header(&h).unwrap().crx_id, id16);
}

#[test]
fn id_and_crc_helpers() {
    assert_eq!(id_from_hash(&[0u8; 32]), "a".repeat(32));
    assert_eq!(id_from_hash(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0xff]),
        "abcdefghijklmnopaaaaaaaaaaaaaapp");
    assert!(is_extension_id("ddkjiahejlhfcafbddmgiahcphecmpfh"));
    assert!(!is_extension_id("ddkjiahejlhfcafbddmgiahcphecmpfz"));
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn ecdsa_der_conversion_is_strict() {
    let r = [0x80u8; 32];
    let s = [0x01u8; 32];
    let der = raw_to_der(&[r, s].concat());
    assert_eq!(ecdsa_der_to_raw(&der, 32).unwrap(), [r, s].concat());
    // Short integers are left-padded.
    assert_eq!(
        ecdsa_der_to_raw(&[0x30, 0x06, 0x02, 0x01, 0x05, 0x02, 0x01, 0x07], 32).unwrap(),
        [vec![0; 31], vec![5], vec![0; 31], vec![7]].concat()
    );
    let bad: &[&[u8]] = &[
        &[],
        &[0x30],
        &[0x31, 0x06, 0x02, 0x01, 0x05, 0x02, 0x01, 0x07], // not a SEQUENCE
        &[0x30, 0x07, 0x02, 0x01, 0x05, 0x02, 0x01, 0x07], // length off
        &[0x30, 0x07, 0x02, 0x01, 0x05, 0x02, 0x01, 0x07, 0x00], // trailing byte
        &[0x30, 0x06, 0x02, 0x01, 0x85, 0x02, 0x01, 0x07], // negative
        &[0x30, 0x07, 0x02, 0x02, 0x00, 0x05, 0x02, 0x01, 0x07], // not minimal
        &[0x30, 0x05, 0x02, 0x00, 0x02, 0x01, 0x07],       // empty integer
        &[0x30, 0x06, 0x04, 0x01, 0x05, 0x02, 0x01, 0x07], // not an INTEGER
        &[0x30, 0x03, 0x02, 0x01, 0x05],                   // no s
    ];
    for b in bad {
        assert!(ecdsa_der_to_raw(b, 32).is_none(), "{b:02x?}");
    }
    // 33 significant bytes is too long for P-256.
    let long = [vec![0x30, 0x26, 0x02, 0x21], vec![0x7f; 33], vec![0x02, 0x01, 0x01]].concat();
    assert!(ecdsa_der_to_raw(&long, 32).is_none());
}

// --- zip reading -------------------------------------------------------------

#[test]
fn reads_sample_zip() {
    let z = sample_zip();
    let e = read_zip(&z).unwrap();
    let names: Vec<_> = e.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
    assert_eq!(names, [("manifest.json", false), ("js", true), ("js/a.js", false), ("img/x.bin", false)]);
}

#[test]
fn rejects_unsafe_names() {
    let bad = [
        "../x", "/abs", "C:/x", "c:x", "a/../../x", "a/./b", "./a", "a\\b", "..\\x", "f:ads",
        "a/b::$DATA", "CON", "nul.txt", "com1", "a/LPT9.log", "Aux.tar.gz", "conin$", "COM¹",
        "con .txt", "dot./x", "sp /x", "x.", "", "a//b", "a/", "/", "//server/share", "a\0b",
        "tab\tx", "q?.js", "s*", "p|q", "<a>", "\"q\"",
        // 8.3 short-name aliases
        "PROGRA~1", "abc~12.txt", "a~1", "ABCDEF~9/x", "js/progra~1.js", "Ab~3x",
    ];
    for name in bad {
        let name = if name == "a/" { "a//" } else { name }; // "a/" alone is a valid folder
        assert!(matches!(zip_err(&[stored(name, b"x")]), CrxError::BadPath(_)), "{name:?}");
    }
    let mut deep = "d/".repeat(MAX_DEPTH);
    deep.push('f');
    assert!(matches!(zip_err(&[stored(&deep, b"x")]), CrxError::BadPath(_)));
    assert!(matches!(zip_err(&[stored(&"n".repeat(MAX_NAME_BYTES + 1), b"x")]), CrxError::BadPath(_)));
    let mut e = stored("x", b"x");
    e.name = vec![0xff, 0xfe];
    assert!(matches!(zip_err(&[e]), CrxError::BadPath(_)));

    for good in [
        "manifest.json", "_locales/en_US/messages.json", "a b/c d.js", "comfy.txt", "CONsole.js",
        "com10", "lpt", "nul_", "x..y", ".hidden", "a~b", "abcdefg~1", "~1", "ab~", "x~y1/z", "ünicode/ğ.js",
    ] {
        assert!(safe_rel_path(good.as_bytes()).is_ok(), "{good}");
    }
}

#[test]
fn rejects_links_and_special_files() {
    let unix = |mode: u32| Ent { made_by: 3 << 8 | 20, attrs: mode << 16, ..stored("x", b"target") };
    assert!(matches!(zip_err(&[unix(0o120_777)]), CrxError::BadPath(_))); // symlink
    assert!(matches!(zip_err(&[unix(0o020_644)]), CrxError::BadPath(_))); // char device
    assert!(matches!(zip_err(&[unix(0o010_644)]), CrxError::BadPath(_))); // fifo
    assert!(read_zip(&zip(&[unix(0o100_644)])).is_ok());
    let reparse = Ent { attrs: 0x400 | 0x20, ..stored("x", b"y") };
    assert!(matches!(zip_err(&[reparse]), CrxError::BadPath(_)));
}

#[test]
fn rejects_duplicates_and_file_folder_clashes() {
    let cases: [&[Ent]; 4] = [
        &[stored("A.js", b"1"), stored("a.JS", b"2")],
        &[stored("x", b"1"), stored("x/y", b"2")],
        &[stored("x/y", b"1"), stored("X", b"2")],
        &[dir("d/"), stored("d", b"2")],
    ];
    for ents in cases {
        assert!(matches!(zip_err(ents), CrxError::BadPath(_)));
    }
    // A folder entry plus files inside it is fine.
    assert!(read_zip(&zip(&[dir("d/"), stored("d/f", b"1"), stored("D/g", b"2")])).is_ok());
}

#[test]
fn rejects_unsupported_zip_features() {
    for flags in [0x0001, 0x0041, 0x2000] {
        let e = Ent { flags, ..stored("x", b"y") };
        assert!(matches!(zip_err(&[e]), CrxError::BadZip("encrypted entry")));
    }
    let e = Ent { method: 12, ..stored("x", b"y") };
    assert!(matches!(zip_err(&[e]), CrxError::BadZip("unsupported compression")));
    let e = Ent { size: 99, ..stored("x", b"y") };
    assert!(matches!(zip_err(&[e]), CrxError::BadZip("stored size mismatch")));
    let e = Ent { size: 5, ..dir("d/") };
    assert!(matches!(zip_err(&[e]), CrxError::BadZip(_)));

    // zip64 markers in the end record.
    let mut z = zip(&[stored("x", b"y")]);
    let eocd = z.len() - EOCD_LEN;
    z[eocd + 8..eocd + 12].copy_from_slice(&[0xff; 4]);
    assert!(matches!(read_zip(&z), Err(CrxError::BadZip("zip64 is not supported"))));
    let mut z = zip(&[stored("x", b"y")]);
    z[eocd + 16..eocd + 20].copy_from_slice(&[0xff; 4]);
    assert!(matches!(read_zip(&z), Err(CrxError::BadZip("zip64 is not supported"))));
    // Multi-disk.
    let mut z = zip(&[stored("x", b"y")]);
    z[eocd + 4] = 1;
    assert!(matches!(read_zip(&z), Err(CrxError::BadZip("multi-disk archive"))));
}

#[test]
fn rejects_malformed_zip_structure() {
    let good = zip(&[stored("a", b"hello"), stored("b", b"world")]);
    let eocd = good.len() - EOCD_LEN;
    assert!(read_zip(&good).is_ok());

    let broken: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("no end record", good[..eocd].to_vec()),
        ("cut end record", good[..good.len() - 1].to_vec()),
        ("trailing junk", [good.clone(), vec![0]].concat()),
        ("central dir past end", {
            let mut z = good.clone();
            z[eocd + 12..eocd + 16].copy_from_slice(&1000u32.to_le_bytes());
            z
        }),
        ("central dir shorter than its entries", {
            let mut z = good.clone();
            let size = le32(&z, eocd + 12) - 1;
            z[eocd + 12..eocd + 16].copy_from_slice(&size.to_le_bytes());
            z
        }),
        ("fewer entries than the directory holds", {
            let mut z = good.clone();
            z[eocd + 8..eocd + 12].copy_from_slice(&[1, 0, 1, 0]);
            z
        }),
        ("more entries than the directory holds", {
            let mut z = good.clone();
            z[eocd + 8..eocd + 12].copy_from_slice(&[3, 0, 3, 0]);
            z
        }),
        ("bad local signature", {
            let mut z = good.clone();
            z[0] = 0;
            z
        }),
        ("local name differs", {
            let mut z = good.clone();
            z[30] = b'z';
            z
        }),
        ("bad central signature", {
            let mut z = good.clone();
            let cd = le32(&z, eocd + 16) as usize;
            z[cd] = 0;
            z
        }),
        ("local offset into the central dir", {
            let mut z = good.clone();
            let cd = le32(&z, eocd + 16) as usize;
            z[cd + 42..cd + 46].copy_from_slice(&(cd as u32).to_le_bytes());
            z
        }),
    ];
    for (what, z) in broken {
        assert!(matches!(read_zip(&z), Err(CrxError::BadZip(_))), "{what}");
    }

    // A comment is fine if its length reaches exactly to the end.
    let mut z = good.clone();
    z[eocd + 20..eocd + 22].copy_from_slice(&3u16.to_le_bytes());
    z.extend(b"abc");
    assert!(read_zip(&z).is_ok());
}

#[test]
fn rejects_overlapping_entries() {
    // "a" claims to run over "b"'s whole record: one byte range, two files.
    let mut a = stored("a", b"hello");
    a.size = 5 + 30 + 1 + 5;
    a.csize = Some(a.size);
    let z = zip(&[a, stored("b", b"world")]);
    assert!(matches!(read_zip(&z), Err(CrxError::BadZip("overlapping entries"))));
}

#[test]
fn enforces_limits() {
    let mut e = deflated("big", b"x");
    e.size = MAX_ENTRY_BYTES as u32 + 1;
    assert!(matches!(zip_err(&[e]), CrxError::Limit("file size")));

    let ents: Vec<_> = (0..5)
        .map(|i| Ent { size: 60 << 20, ..deflated(&format!("f{i}"), b"x") })
        .collect();
    assert!(matches!(zip_err(&ents), CrxError::Limit("total size")));

    let mut z = zip(&[stored("x", b"y")]);
    let eocd = z.len() - EOCD_LEN;
    let n = (MAX_ENTRIES as u16 + 1).to_le_bytes();
    z[eocd + 8..eocd + 10].copy_from_slice(&n);
    z[eocd + 10..eocd + 12].copy_from_slice(&n);
    assert!(matches!(read_zip(&z), Err(CrxError::Limit("entry count"))));
}

// --- policy ------------------------------------------------------------------

#[test]
fn web_store_policy_needs_the_publisher_proof() {
    let (rsa, ec) = (TestKey::rsa(), TestKey::p256());
    let z = sample_zip();
    let c = crx(&z, &[&rsa]);
    assert!(matches!(verify_crx3(&c, &rsa.id(), Policy::WebStore), Err(CrxError::RequiredProofMissing)));
    // Any number of other valid proofs is no substitute.
    let c = crx(&z, &[&rsa, &ec]);
    assert!(matches!(verify_crx3(&c, &rsa.id(), Policy::WebStore), Err(CrxError::RequiredProofMissing)));
    // The production publisher key, never Chrome's test publisher key (6c46413b...).
    assert_eq!(PUBLISHER_KEY_HASH[..4], [0x61, 0xf7, 0xf2, 0xa6]);
    assert_ne!(PUBLISHER_KEY_HASH[..4], [0x6c, 0x46, 0x41, 0x3b]);
}

#[test]
fn rsa_key_size_depends_on_policy() {
    let small = TestKey::rsa1024();
    let c = crx(&sample_zip(), &[&small]);
    assert!(matches!(verify_crx3(&c, &small.id(), Policy::SelfSigned), Err(CrxError::BadKey)));
    // Web Store developer keys can be 1024-bit: the key passes, and only the
    // missing publisher proof stops it.
    assert!(matches!(verify_crx3(&c, &small.id(), Policy::WebStore), Err(CrxError::RequiredProofMissing)));
}

// --- unpack ------------------------------------------------------------------

const DEV_KEY: &[u8] = b"developer public key";

/// What `verify_crx3` hands over, for unpack tests that need no signature.
fn payload(zip: &[u8]) -> ZipPayload<'_> {
    ZipPayload { zip, id: String::new(), key_hashes: vec![], developer_key: DEV_KEY }
}

fn manifest_at(dest: &Path) -> serde_json::Map<String, serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(dest.join("manifest.json")).unwrap()).unwrap();
    v.as_object().unwrap().clone()
}

#[test]
fn base64_matches_rfc_4648() {
    for (raw, enc) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(raw.as_bytes()), enc);
    }
    assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
}

#[test]
fn unpacks_into_new_or_empty_folder() {
    let root = temp_dir("crx-unpack");
    let z = sample_zip();
    for dest in [root.join("new"), root.join("empty")] {
        if dest.ends_with("empty") {
            fs::create_dir(&dest).unwrap();
        }
        assert_eq!(unpack(&payload(&z), &dest).unwrap(), 3);
        let m = manifest_at(&dest);
        assert_eq!((m["manifest_version"].as_i64(), m["name"].as_str()), (Some(3), Some("t")));
        assert_eq!(m["key"], base64(DEV_KEY));
        assert_eq!(m.len(), 4);
        assert_eq!(fs::read(dest.join("js").join("a.js")).unwrap(), b"console.log(1);\n".repeat(200));
        assert_eq!(fs::read(dest.join("img").join("x.bin")).unwrap(), [0, 1, 2, 3, 255]);
    }
    assert_no_temp_left(&root);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unpack_gives_the_manifest_the_developer_key() {
    // A signed package whose manifest brings someone else's key.
    let (dev, other) = (TestKey::rsa(), TestKey::p256());
    let foreign = base64(&other.spki);
    let manifest = format!(r#"{{"manifest_version":3,"name":"t","version":"1","key":"{foreign}"}}"#);
    let z = zip(&[deflated("manifest.json", manifest.as_bytes()), stored("a.js", b"1")]);
    let c = crx(&z, &[&dev]);
    let p = verify_crx3(&c, &dev.id(), Policy::SelfSigned).unwrap();
    let root = temp_dir("crx-key");
    let dest = root.join("ext");
    assert_eq!(unpack(&p, &dest).unwrap(), 2);
    let m = manifest_at(&dest);
    assert_eq!(m["key"], base64(&dev.spki));
    assert_ne!(m["key"], foreign);
    assert_eq!(m["name"], "t");

    // A UTF-8 BOM and a differently cased name.
    let z = zip(&[stored("Manifest.JSON", b"\xef\xbb\xbf{\"name\":\"b\"}")]);
    let dest = root.join("bom");
    unpack(&payload(&z), &dest).unwrap();
    let m = manifest_at(&dest);
    assert_eq!((m["name"].as_str(), m["key"].as_str()), (Some("b"), Some(&*base64(DEV_KEY))));
    assert_no_temp_left(&root);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unpack_refuses_a_missing_or_bad_manifest() {
    let root = temp_dir("crx-manifest");
    let cases: Vec<(&str, Vec<Ent>)> = vec![
        ("missing", vec![stored("a.js", b"1")]),
        ("only nested", vec![stored("sub/manifest.json", b"{}")]),
        ("a folder", vec![dir("manifest.json/"), stored("manifest.json/x", b"{}")]),
        ("array", vec![stored("manifest.json", b"[1,2]")]),
        ("string", vec![stored("manifest.json", b"\"x\"")]),
        ("not json", vec![stored("manifest.json", b"{name:")]),
        ("empty", vec![stored("manifest.json", b"")]),
    ];
    for (what, ents) in cases {
        let z = zip(&ents);
        let dest = root.join(what);
        let err = unpack(&payload(&z), &dest).unwrap_err();
        assert!(matches!(err, CrxError::BadManifest(_)), "{what}: {err}");
        assert!(!dest.exists(), "{what}");
    }
    assert_no_temp_left(&root);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unpack_leaves_a_non_empty_destination_alone() {
    let root = temp_dir("crx-nonempty");
    let dest = root.join("ext");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("keep.txt"), b"mine").unwrap();
    let z = sample_zip();
    assert!(matches!(unpack(&payload(&z), &dest), Err(CrxError::DestNotEmpty)));
    let file = root.join("file");
    fs::write(&file, b"x").unwrap();
    assert!(matches!(unpack(&payload(&z), &file), Err(CrxError::DestNotEmpty)));
    assert_eq!(fs::read(dest.join("keep.txt")).unwrap(), b"mine");
    assert_eq!(fs::read_dir(&dest).unwrap().count(), 1);
    assert_no_temp_left(&root);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unpack_failure_leaves_nothing_behind() {
    let root = temp_dir("crx-fail");
    let manifest = stored("manifest.json", b"{}");
    let good = deflated("a.js", b"fine");
    let cases: Vec<(&str, Ent)> = vec![
        ("crc", Ent { crc: 1, ..deflated("b.js", b"bad crc") }),
        ("bomb", Ent { size: 10, ..deflated("b.js", &[0u8; 100_000]) }),
        ("short", Ent { size: 50, ..deflated("b.js", b"three") }),
        ("garbage deflate", Ent { data: vec![0xff; 16], ..deflated("b.js", b"x") }),
    ];
    for (what, bad) in cases {
        let dest = root.join(what);
        let z = zip(&[manifest.clone(), good.clone(), bad]);
        let err = unpack(&payload(&z), &dest).unwrap_err();
        assert!(matches!(err, CrxError::BadZip(_)), "{what}: {err}");
        assert!(!dest.exists(), "{what}");
    }
    // A refused name: nothing is written at all.
    let dest = root.join("slip");
    let z = zip(&[manifest.clone(), good.clone(), stored("../evil", b"x")]);
    assert!(matches!(unpack(&payload(&z), &dest), Err(CrxError::BadPath(_))));
    assert!(!dest.exists() && !root.parent().unwrap().join("evil").exists());
    assert_no_temp_left(&root);
    let _ = fs::remove_dir_all(&root);
}

fn assert_no_temp_left(root: &Path) {
    let left: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".unpack-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

// --- a real package ----------------------------------------------------------

/// `XB_CRX_FIXTURE=<path to .crx> XB_CRX_ID=<id> cargo test real_crx -- --ignored --nocapture`
#[test]
#[ignore = "needs a downloaded CRX (XB_CRX_FIXTURE, XB_CRX_ID)"]
fn real_crx() {
    let path = std::env::var("XB_CRX_FIXTURE").expect("XB_CRX_FIXTURE");
    let id = std::env::var("XB_CRX_ID").expect("XB_CRX_ID");
    let bytes = fs::read(path).unwrap();
    let p = verify_crx3(&bytes, &id, Policy::WebStore).unwrap();
    for h in &p.key_hashes {
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        println!("proof key sha256 {hex} (id {})", id_from_hash(h));
    }
    assert!(p.key_hashes.contains(&PUBLISHER_KEY_HASH));
    assert_eq!(id_from_hash(&cng::sha256(&[p.developer_key]).unwrap()), id);
    let root = temp_dir("crx-real");
    let dest = root.join("ext");
    let files = unpack(&p, &dest).unwrap();
    println!("{files} files");
    assert_eq!(manifest_at(&dest)["key"], base64(p.developer_key));
    let _ = fs::remove_dir_all(&root);
}
