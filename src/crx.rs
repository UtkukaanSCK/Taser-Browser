//! CRX3 packages (v0.3, core of task 4): check that a downloaded Chrome Web
//! Store package is signed by the extension it claims to be, then unpack its
//! zip without letting it write anywhere else. No WebView2 here.
//!
//! - `verify_crx3` follows Chromium's `crx_file::Verify` (CRX3 only):
//!   `"Cr24"`, version 3, a `CrxFileHeader` protobuf, the extension id from
//!   `SignedData.crx_id`, at least one proof whose public key hashes to that
//!   id, and every proof (RSA and ECDSA P-256) valid over
//!   `"CRX3 SignedData\0" + le_u32(len) + signed_header_data + zip`.
//! - SHA-256 and the signature checks are Windows' own (CNG, through the
//!   `windows` crate already in the build), not a crypto crate.
//! - `unpack` reads the whole zip directory first and refuses the package on
//!   any unsafe name (absolute, `..`, drive, `:` streams, reserved device
//!   names, case-insensitive duplicates), any link, encryption, zip64, or an
//!   entry over the size limits. Only then does it write, into a fresh temp
//!   folder next to the destination, which it renames into place.
//! - Everything here is untrusted input: every length is checked before use
//!   and nothing panics on bad bytes.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Largest package accepted (bytes), before anything is parsed.
pub const MAX_CRX_BYTES: usize = 64 << 20;
/// Largest CRX header (the proofs and the signed data).
pub const MAX_HEADER_BYTES: usize = 256 << 10;
/// Most proofs in one header (Web Store packages carry two).
const MAX_PROOFS: usize = 16;
/// Most zip entries.
pub const MAX_ENTRIES: usize = 20_000;
/// Largest single file once unpacked.
pub const MAX_ENTRY_BYTES: u64 = 64 << 20;
/// Largest total once unpacked.
pub const MAX_TOTAL_BYTES: u64 = 256 << 20;
/// Longest entry name (bytes) and deepest folder nesting.
const MAX_NAME_BYTES: usize = 512;
const MAX_DEPTH: usize = 32;

const MAGIC: &[u8; 4] = b"Cr24";
const SIGNED_CONTEXT: &[u8] = b"CRX3 SignedData\x00";

#[derive(Debug)]
pub enum CrxError {
    /// The package is over `MAX_CRX_BYTES`.
    TooLarge,
    /// Not a CRX at all.
    BadMagic,
    /// A CRX, but not version 3.
    BadVersion(u32),
    /// The header is truncated, oversized or not the expected protobuf.
    BadHeader(&'static str),
    /// The caller's id is not 32 letters a–p.
    BadExpectedId,
    /// The package names a different extension.
    IdMismatch,
    /// No proof is signed by the key the id comes from.
    NoDeveloperProof,
    /// The policy needs a proof (the Web Store's) that the package lacks.
    RequiredProofMissing,
    /// No `manifest.json` file, or not a JSON object.
    BadManifest(&'static str),
    /// A proof's key is not an acceptable RSA or P-256 key.
    BadKey,
    /// A proof's signature does not match.
    BadSignature,
    /// The OS hash call failed.
    Crypto,
    /// The zip inside is malformed or uses a feature not supported here.
    BadZip(&'static str),
    /// An entry name that could escape the folder or alias another file.
    BadPath(&'static str),
    /// Over one of the size or count limits.
    Limit(&'static str),
    /// `unpack`'s destination exists and is not an empty folder.
    DestNotEmpty,
    Io(io::Error),
}

impl fmt::Display for CrxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "package is too large"),
            Self::BadMagic => write!(f, "not a CRX package"),
            Self::BadVersion(v) => write!(f, "unsupported CRX version {v}"),
            Self::BadHeader(why) => write!(f, "bad CRX header: {why}"),
            Self::BadExpectedId => write!(f, "not an extension id"),
            Self::IdMismatch => write!(f, "package is for a different extension"),
            Self::NoDeveloperProof => write!(f, "package is not signed by its extension's key"),
            Self::RequiredProofMissing => write!(f, "package is not signed by the Chrome Web Store"),
            Self::BadManifest(why) => write!(f, "bad manifest.json: {why}"),
            Self::BadKey => write!(f, "unsupported signing key"),
            Self::BadSignature => write!(f, "signature does not match"),
            Self::Crypto => write!(f, "hashing failed"),
            Self::BadZip(why) => write!(f, "bad zip: {why}"),
            Self::BadPath(why) => write!(f, "unsafe file name in package: {why}"),
            Self::Limit(what) => write!(f, "package exceeds the {what} limit"),
            Self::DestNotEmpty => write!(f, "destination folder is not empty"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for CrxError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// SHA-256 of the Chrome Web Store's publisher key (Chromium's
/// `kPublisherKeyHash`, the production "ecdsa_2017_public" key in
/// `components/crx_file/crx_verifier.cc`). A Web Store package carries a
/// proof by this key. Chromium's publisher *test* key (6c46413b…) is
/// deliberately not accepted.
pub const PUBLISHER_KEY_HASH: [u8; 32] = [
    0x61, 0xf7, 0xf2, 0xa6, 0xbf, 0xcf, 0x74, 0xcd, 0x0b, 0xc1, 0xfe, 0x24, 0x97, 0xcc, 0x9b, 0x04,
    0x25, 0x4c, 0x65, 0x8f, 0x79, 0xf2, 0x14, 0x53, 0x92, 0x86, 0x7e, 0xa8, 0x36, 0x63, 0x67, 0xcf,
];

/// Which packages `verify_crx3` accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// From the Chrome Web Store: the developer's proof and the store's
    /// publisher proof (`PUBLISHER_KEY_HASH`). RSA keys from 1024 bits, as
    /// in Chromium; the publisher proof is the real gate.
    WebStore,
    /// Tests and the self-test build only: self-signed packages (developer
    /// proof alone), RSA keys from 2048 bits. A normal build has no such
    /// variant, so it cannot accept a package the store did not sign.
    #[cfg(any(test, feature = "selftest"))]
    SelfSigned,
}

impl Policy {
    fn min_rsa_bits(self) -> u32 {
        match self {
            Self::WebStore => 1024,
            #[cfg(any(test, feature = "selftest"))]
            Self::SelfSigned => 2048,
        }
    }
}

/// A verified package: the zip to unpack and what vouched for it. Only
/// `verify_crx3` makes one, so `unpack` only ever sees checked bytes.
#[derive(Debug)]
pub struct ZipPayload<'a> {
    zip: &'a [u8],
    /// The extension id (equal to the one asked for). Only the tests read
    /// it back; the install flow already has the id it asked for.
    #[cfg_attr(not(test), allow(dead_code))]
    id: String,
    /// SHA-256 of each proof's public key (DER SubjectPublicKeyInfo); every
    /// one of these proofs was checked. Only the tests read it back.
    #[cfg_attr(not(test), allow(dead_code))]
    key_hashes: Vec<[u8; 32]>,
    /// The developer's public key (DER SubjectPublicKeyInfo): the proof key
    /// the id comes from. `unpack` writes it into the manifest as `"key"`.
    developer_key: &'a [u8],
}

impl ZipPayload<'_> {
    #[cfg(test)]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[cfg(test)]
    pub fn key_hashes(&self) -> &[[u8; 32]] {
        &self.key_hashes
    }

    /// The bytes of file `name` in the package (`/`-separated, matched
    /// without regard to case), read before anything is unpacked: the
    /// install question shows the manifest's name and permissions. None if
    /// there is no such file. `Err(TooLarge)` if it is over `max` bytes.
    pub fn file(&self, name: &str, max: u64) -> Result<Option<Vec<u8>>, CrxError> {
        let entries = read_zip(self.zip)?;
        let Some(e) = entries.iter().find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name)) else { return Ok(None) };
        if e.size > max {
            return Err(CrxError::TooLarge);
        }
        Ok(Some(entry_bytes(e)?.into_owned()))
    }
}

/// The id Chrome derives from a key hash (or a `crx_id`): the first 16
/// bytes, each nibble written as a letter `a`–`p`.
pub fn id_from_hash(hash: &[u8]) -> String {
    hash.iter()
        .take(16)
        .flat_map(|b| [b >> 4, b & 15])
        .map(|n| char::from(b'a' + n))
        .collect()
}

/// SHA-256 through Windows CNG (also used for manifest keys, see `extensions`).
pub use cng::sha256;

/// 32 letters `a`–`p`.
pub fn is_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| (b'a'..=b'p').contains(&c))
}

/// Checks a CRX3 package against the extension id it was downloaded for,
/// under `policy`.
pub fn verify_crx3<'a>(
    bytes: &'a [u8],
    expected_id: &str,
    policy: Policy,
) -> Result<ZipPayload<'a>, CrxError> {
    if !is_extension_id(expected_id) {
        return Err(CrxError::BadExpectedId);
    }
    if bytes.len() > MAX_CRX_BYTES {
        return Err(CrxError::TooLarge);
    }
    if bytes.len() < 4 || &bytes[..4] != MAGIC {
        return Err(CrxError::BadMagic);
    }
    if bytes.len() < 12 {
        return Err(CrxError::BadHeader("truncated"));
    }
    let version = le32(bytes, 4);
    if version != 3 {
        return Err(CrxError::BadVersion(version));
    }
    let header_len = le32(bytes, 8) as usize;
    if header_len > MAX_HEADER_BYTES {
        return Err(CrxError::BadHeader("header too large"));
    }
    let Some(header) = bytes.get(12..12 + header_len) else {
        return Err(CrxError::BadHeader("truncated"));
    };
    let zip = &bytes[12 + header_len..];
    if zip.is_empty() {
        return Err(CrxError::BadZip("empty archive"));
    }

    let header = parse_header(header)?;
    if id_from_hash(&header.crx_id) != expected_id {
        return Err(CrxError::IdMismatch);
    }
    let key_hashes = header
        .proofs
        .iter()
        .map(|p| cng::sha256(&[p.key]).ok_or(CrxError::Crypto))
        .collect::<Result<Vec<_>, _>>()?;
    let Some(developer) = key_hashes.iter().position(|h| id_from_hash(h) == expected_id) else {
        return Err(CrxError::NoDeveloperProof);
    };
    let signed_len = (header.signed_data.len() as u32).to_le_bytes();
    let digest = cng::sha256(&[SIGNED_CONTEXT, &signed_len, header.signed_data, zip])
        .ok_or(CrxError::Crypto)?;
    // Like Chromium: every proof must hold, not just the developer's.
    for p in &header.proofs {
        cng::verify(p.alg, p.key, p.sig, &digest, policy.min_rsa_bits())?;
    }
    if policy == Policy::WebStore && !key_hashes.contains(&PUBLISHER_KEY_HASH) {
        return Err(CrxError::RequiredProofMissing);
    }
    Ok(ZipPayload {
        zip,
        id: expected_id.to_owned(),
        key_hashes,
        developer_key: header.proofs[developer].key,
    })
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

// ---------------------------------------------------------------------------
// The CRX header: a few protobuf fields, read by hand.
//
// message CrxFileHeader {
//   repeated AsymmetricKeyProof sha256_with_rsa = 2;
//   repeated AsymmetricKeyProof sha256_with_ecdsa = 3;
//   optional bytes signed_header_data = 10000;   // a SignedData
// }
// message AsymmetricKeyProof { optional bytes public_key = 1; optional bytes signature = 2; }
// message SignedData { optional bytes crx_id = 1; }   // 16 bytes
//
// Other fields are skipped. A repeated singular field is refused rather than
// resolved "last one wins", so no two readers can disagree on what was signed.

#[derive(Clone, Copy, Debug, PartialEq)]
enum Alg {
    Rsa,
    EcdsaP256,
}

struct Proof<'a> {
    alg: Alg,
    key: &'a [u8],
    sig: &'a [u8],
}

struct Header<'a> {
    proofs: Vec<Proof<'a>>,
    signed_data: &'a [u8],
    crx_id: [u8; 16],
}

enum Value<'a> {
    Scalar,
    Bytes(&'a [u8]),
}

struct Pb<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Pb<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn varint(&mut self) -> Result<u64, CrxError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(&byte) = self.buf.get(self.at) else {
                return Err(CrxError::BadHeader("truncated varint"));
            };
            self.at += 1;
            if shift == 63 && byte > 1 {
                return Err(CrxError::BadHeader("varint overflow"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(CrxError::BadHeader("varint too long"))
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], CrxError> {
        let rest = self.buf.len() - self.at;
        if n > rest as u64 {
            return Err(CrxError::BadHeader("field runs past the end"));
        }
        let out = &self.buf[self.at..self.at + n as usize];
        self.at += n as usize;
        Ok(out)
    }

    /// The next (field number, value), or `None` at the end.
    fn field(&mut self) -> Result<Option<(u64, Value<'a>)>, CrxError> {
        if self.at == self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let number = key >> 3;
        if number == 0 {
            return Err(CrxError::BadHeader("field number 0"));
        }
        let value = match key & 7 {
            0 => {
                self.varint()?;
                Value::Scalar
            }
            1 => {
                self.take(8)?;
                Value::Scalar
            }
            2 => {
                let len = self.varint()?;
                Value::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Value::Scalar
            }
            _ => return Err(CrxError::BadHeader("unsupported wire type")),
        };
        Ok(Some((number, value)))
    }
}

fn parse_header(buf: &[u8]) -> Result<Header<'_>, CrxError> {
    let mut pb = Pb::new(buf);
    let mut proofs = Vec::new();
    let mut signed_data = None;
    while let Some((number, value)) = pb.field()? {
        let alg = match number {
            2 => Alg::Rsa,
            3 => Alg::EcdsaP256,
            10000 => {
                let Value::Bytes(b) = value else {
                    return Err(CrxError::BadHeader("signed data is not bytes"));
                };
                if signed_data.replace(b).is_some() {
                    return Err(CrxError::BadHeader("signed data repeated"));
                }
                continue;
            }
            _ => continue,
        };
        let Value::Bytes(b) = value else {
            return Err(CrxError::BadHeader("proof is not a message"));
        };
        if proofs.len() == MAX_PROOFS {
            return Err(CrxError::BadHeader("too many proofs"));
        }
        proofs.push(parse_proof(b, alg)?);
    }
    let Some(signed_data) = signed_data else {
        return Err(CrxError::BadHeader("no signed data"));
    };
    let crx_id = parse_signed_data(signed_data)?;
    Ok(Header { proofs, signed_data, crx_id })
}

fn parse_proof(buf: &[u8], alg: Alg) -> Result<Proof<'_>, CrxError> {
    let mut pb = Pb::new(buf);
    let (mut key, mut sig) = (None, None);
    while let Some((number, value)) = pb.field()? {
        let slot = match number {
            1 => &mut key,
            2 => &mut sig,
            _ => continue,
        };
        let Value::Bytes(b) = value else {
            return Err(CrxError::BadHeader("proof field is not bytes"));
        };
        if slot.replace(b).is_some() {
            return Err(CrxError::BadHeader("proof field repeated"));
        }
    }
    match (key, sig) {
        (Some(key), Some(sig)) if !key.is_empty() && !sig.is_empty() => Ok(Proof { alg, key, sig }),
        _ => Err(CrxError::BadHeader("proof without key or signature")),
    }
}

fn parse_signed_data(buf: &[u8]) -> Result<[u8; 16], CrxError> {
    let mut pb = Pb::new(buf);
    let mut id = None;
    while let Some((number, value)) = pb.field()? {
        if number != 1 {
            continue;
        }
        let Value::Bytes(b) = value else {
            return Err(CrxError::BadHeader("crx id is not bytes"));
        };
        if id.replace(b).is_some() {
            return Err(CrxError::BadHeader("crx id repeated"));
        }
    }
    id.and_then(|b| <[u8; 16]>::try_from(b).ok())
        .ok_or(CrxError::BadHeader("crx id missing or not 16 bytes"))
}

/// A DER `ECDSA-Sig-Value` (SEQUENCE { INTEGER r, INTEGER s }), as CRX and
/// Chromium carry it, to the fixed `r || s` CNG expects. Strict DER only.
fn ecdsa_der_to_raw(der: &[u8], half: usize) -> Option<Vec<u8>> {
    fn int<'a>(b: &'a [u8], at: &mut usize, half: usize) -> Option<&'a [u8]> {
        if *b.get(*at)? != 0x02 {
            return None;
        }
        let len = usize::from(*b.get(*at + 1)?);
        if len == 0 || len > 0x7f {
            return None;
        }
        let v = b.get(*at + 2..*at + 2 + len)?;
        *at += 2 + len;
        if v[0] & 0x80 != 0 {
            return None; // negative
        }
        let v = if v[0] == 0 && len > 1 {
            if v[1] & 0x80 == 0 {
                return None; // not minimal
            }
            &v[1..]
        } else {
            v
        };
        (v.len() <= half).then_some(v)
    }
    if der.len() < 2 || der[0] != 0x30 || usize::from(der[1]) != der.len() - 2 || der[1] > 0x7f {
        return None;
    }
    let mut at = 2;
    let r = int(der, &mut at, half)?;
    let s = int(der, &mut at, half)?;
    if at != der.len() {
        return None;
    }
    let mut out = vec![0u8; half * 2];
    out[half - r.len()..half].copy_from_slice(r);
    out[2 * half - s.len()..].copy_from_slice(s);
    Some(out)
}

/// Windows CNG: SHA-256 and the proof checks.
mod cng {
    use super::{Alg, CrxError, ecdsa_der_to_raw};
    use core::ffi::c_void;
    use windows::Win32::Security::Cryptography::{
        BCRYPT_FLAGS, BCRYPT_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_KEY_HANDLE, BCRYPT_KEY_STRENGTH,
        BCRYPT_PAD_PKCS1, BCRYPT_PKCS1_PADDING_INFO, BCRYPT_SHA256_ALG_HANDLE,
        BCRYPT_SHA256_ALGORITHM, BCryptCreateHash, BCryptDestroyHash, BCryptDestroyKey,
        BCryptFinishHash, BCryptGetProperty, BCryptHashData, BCryptVerifySignature,
        CERT_PUBLIC_KEY_INFO, CRYPT_IMPORT_PUBLIC_KEY_FLAGS, CryptDecodeObjectEx,
        CryptImportPublicKeyInfoEx2, X509_ASN_ENCODING, X509_PUBLIC_KEY_INFO,
    };

    const OID_RSA: &[u8] = b"1.2.840.113549.1.1.1";
    const OID_EC: &[u8] = b"1.2.840.10045.2.1";
    /// DER of the named-curve OID prime256v1 (1.2.840.10045.3.1.7).
    const P256_PARAMS: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    /// Largest RSA key; the smallest depends on the policy.
    const RSA_MAX_BITS: u32 = 8192;

    /// SHA-256 of the parts, in order. Each part is under 4 GiB (the
    /// package limit), which CNG's u32 lengths need.
    pub fn sha256(parts: &[&[u8]]) -> Option<[u8; 32]> {
        let mut out = [0u8; 32];
        // SAFETY: plain CNG calls on a pseudo-handle; the hash handle is
        // destroyed on every path and the buffers outlive the calls.
        unsafe {
            let mut h = BCRYPT_HASH_HANDLE::default();
            if BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0).is_err() {
                return None;
            }
            let ok = parts.iter().all(|p| BCryptHashData(h, p, 0).is_ok())
                && BCryptFinishHash(h, &mut out, 0).is_ok();
            let _ = BCryptDestroyHash(h);
            ok.then_some(out)
        }
    }

    struct Key(BCRYPT_KEY_HANDLE);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: a handle from CryptImportPublicKeyInfoEx2, freed once.
            unsafe {
                let _ = BCryptDestroyKey(self.0);
            }
        }
    }

    /// A DER SubjectPublicKeyInfo as a CNG key, if it is of the expected
    /// kind: RSA (`min_rsa_bits`–8192 bits) or ECDSA on P-256.
    fn import(spki: &[u8], alg: Alg, min_rsa_bits: u32) -> Option<Key> {
        // SAFETY: the decoded structure lives in `buf` (u64-aligned, sized
        // by the first call) and is only read while `buf` is alive; its
        // pointers point into `buf`.
        unsafe {
            let mut len = 0u32;
            CryptDecodeObjectEx(X509_ASN_ENCODING, X509_PUBLIC_KEY_INFO, spki, 0, None, None, &mut len)
                .ok()?;
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            CryptDecodeObjectEx(
                X509_ASN_ENCODING,
                X509_PUBLIC_KEY_INFO,
                spki,
                0,
                None,
                Some(buf.as_mut_ptr().cast()),
                &mut len,
            )
            .ok()?;
            if (len as usize) > buf.len() * 8 || (len as usize) < size_of::<CERT_PUBLIC_KEY_INFO>() {
                return None;
            }
            let info = &*buf.as_ptr().cast::<CERT_PUBLIC_KEY_INFO>();
            let oid = info.Algorithm.pszObjId;
            if oid.is_null() {
                return None;
            }
            let oid = oid.as_bytes();
            match alg {
                Alg::Rsa if oid == OID_RSA => {}
                Alg::EcdsaP256 if oid == OID_EC => {
                    let p = &info.Algorithm.Parameters;
                    if p.pbData.is_null()
                        || std::slice::from_raw_parts(p.pbData, p.cbData as usize) != P256_PARAMS
                    {
                        return None;
                    }
                }
                _ => return None,
            }
            let mut h = BCRYPT_KEY_HANDLE::default();
            CryptImportPublicKeyInfoEx2(X509_ASN_ENCODING, info, CRYPT_IMPORT_PUBLIC_KEY_FLAGS(0), None, &mut h)
                .ok()?;
            let key = Key(h);
            let mut bits = [0u8; 4];
            let mut got = 0u32;
            if BCryptGetProperty(BCRYPT_HANDLE(key.0.0), BCRYPT_KEY_STRENGTH, Some(&mut bits), &mut got, 0)
                .is_err()
                || got != 4
            {
                return None;
            }
            let bits = u32::from_ne_bytes(bits);
            let fits = match alg {
                Alg::Rsa => (min_rsa_bits..=RSA_MAX_BITS).contains(&bits),
                Alg::EcdsaP256 => bits == 256,
            };
            fits.then_some(key)
        }
    }

    /// One proof: `sig` by `spki` over the message whose SHA-256 is `digest`.
    pub fn verify(
        alg: Alg,
        spki: &[u8],
        sig: &[u8],
        digest: &[u8; 32],
        min_rsa_bits: u32,
    ) -> Result<(), CrxError> {
        let key = import(spki, alg, min_rsa_bits).ok_or(CrxError::BadKey)?;
        // SAFETY: the key handle is alive (`key`), the padding info and its
        // static algorithm name outlive the call.
        let ok = unsafe {
            match alg {
                Alg::Rsa => {
                    let pad = BCRYPT_PKCS1_PADDING_INFO { pszAlgId: BCRYPT_SHA256_ALGORITHM };
                    BCryptVerifySignature(
                        key.0,
                        Some((&raw const pad).cast::<c_void>()),
                        digest,
                        sig,
                        BCRYPT_PAD_PKCS1,
                    )
                    .is_ok()
                }
                Alg::EcdsaP256 => match ecdsa_der_to_raw(sig, 32) {
                    Some(raw) => BCryptVerifySignature(key.0, None, digest, &raw, BCRYPT_FLAGS(0)).is_ok(),
                    None => false,
                },
            }
        };
        if ok { Ok(()) } else { Err(CrxError::BadSignature) }
    }
}

// ---------------------------------------------------------------------------
// The zip: read by hand from the central directory, so every name, size,
// flag and offset is checked before a single byte is written.

/// One file or folder of a checked zip.
#[derive(Debug)]
struct ZipEntry<'a> {
    /// Relative path, `/`-separated, already checked (`safe_rel_path`).
    pub name: String,
    pub is_dir: bool,
    method: u16,
    crc: u32,
    size: u64,
    data: &'a [u8],
}

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;
const CENTRAL_LEN: usize = 46;
const LOCAL_LEN: usize = 30;

/// Every entry of `zip`, or the first reason it is refused.
fn read_zip(zip: &[u8]) -> Result<Vec<ZipEntry<'_>>, CrxError> {
    let eocd = find_eocd(zip).ok_or(CrxError::BadZip("no end of central directory"))?;
    let e = &zip[eocd..];
    if le16(e, 4) != 0 || le16(e, 6) != 0 || le16(e, 8) != le16(e, 10) {
        return Err(CrxError::BadZip("multi-disk archive"));
    }
    let count = le16(e, 10);
    let (cd_size, cd_offset) = (le32(e, 12), le32(e, 16));
    if count == 0xffff || cd_size == 0xffff_ffff || cd_offset == 0xffff_ffff {
        return Err(CrxError::BadZip("zip64 is not supported"));
    }
    let count = usize::from(count);
    if count > MAX_ENTRIES {
        return Err(CrxError::Limit("entry count"));
    }
    let cd_start = cd_offset as usize;
    let Some(cd_end) = cd_start.checked_add(cd_size as usize) else {
        return Err(CrxError::BadZip("central directory out of range"));
    };
    if cd_end > eocd {
        return Err(CrxError::BadZip("central directory out of range"));
    }

    let mut entries = Vec::with_capacity(count);
    let mut spans = Vec::with_capacity(count);
    let mut total = 0u64;
    let mut at = cd_start;
    for _ in 0..count {
        let Some(c) = zip.get(at..at + CENTRAL_LEN).filter(|_| at + CENTRAL_LEN <= cd_end) else {
            return Err(CrxError::BadZip("central directory truncated"));
        };
        if le32(c, 0) != CENTRAL_SIG {
            return Err(CrxError::BadZip("bad central header"));
        }
        let made_by_os = c[5];
        let flags = le16(c, 8);
        let method = le16(c, 10);
        let crc = le32(c, 16);
        let (csize, size) = (u64::from(le32(c, 20)), u64::from(le32(c, 24)));
        let (name_len, extra_len, comment_len) =
            (usize::from(le16(c, 28)), usize::from(le16(c, 30)), usize::from(le16(c, 32)));
        let disk = le16(c, 34);
        let attrs = le32(c, 38);
        let local = le32(c, 42) as usize;
        let name_raw = zip
            .get(at + CENTRAL_LEN..at + CENTRAL_LEN + name_len)
            .ok_or(CrxError::BadZip("central directory truncated"))?;
        at += CENTRAL_LEN + name_len + extra_len + comment_len;
        if at > cd_end {
            return Err(CrxError::BadZip("central directory truncated"));
        }

        if flags & 0x0001 != 0 || flags & 0x0040 != 0 || flags & 0x2000 != 0 {
            return Err(CrxError::BadZip("encrypted entry"));
        }
        if method != 0 && method != 8 {
            return Err(CrxError::BadZip("unsupported compression"));
        }
        if disk != 0 || csize == 0xffff_ffff || size == 0xffff_ffff || local == 0xffff_ffff {
            return Err(CrxError::BadZip("zip64 is not supported"));
        }
        if is_link_or_special(made_by_os, attrs) {
            return Err(CrxError::BadPath("link or special file"));
        }
        let (name, is_dir) = safe_rel_path(name_raw).map_err(CrxError::BadPath)?;
        if is_dir && size != 0 {
            return Err(CrxError::BadZip("folder with contents"));
        }
        if method == 0 && csize != size {
            return Err(CrxError::BadZip("stored size mismatch"));
        }
        if size > MAX_ENTRY_BYTES {
            return Err(CrxError::Limit("file size"));
        }
        total += size;
        if total > MAX_TOTAL_BYTES {
            return Err(CrxError::Limit("total size"));
        }

        // The local header must agree on the name and lie before the
        // central directory, with its data too.
        let l = local
            .checked_add(LOCAL_LEN)
            .filter(|&end| end <= cd_start)
            .and_then(|end| zip.get(local..end))
            .ok_or(CrxError::BadZip("local header out of range"))?;
        if le32(l, 0) != LOCAL_SIG {
            return Err(CrxError::BadZip("bad local header"));
        }
        let (lname_len, lextra_len) = (usize::from(le16(l, 26)), usize::from(le16(l, 28)));
        // `local + LOCAL_LEN <= cd_start` above, so only the data sizes can
        // overflow here.
        let data_start = (local + LOCAL_LEN).checked_add(lname_len + lextra_len);
        let Some((data_start, data_end)) =
            data_start.and_then(|s| Some((s, s.checked_add(csize as usize)?)))
        else {
            return Err(CrxError::BadZip("entry data out of range"));
        };
        if data_end > cd_start {
            return Err(CrxError::BadZip("entry data out of range"));
        }
        if zip[local + LOCAL_LEN..local + LOCAL_LEN + lname_len] != *name_raw {
            return Err(CrxError::BadZip("local and central names differ"));
        }
        spans.push((local, data_end));
        entries.push(ZipEntry { name, is_dir, method, crc, size, data: &zip[data_start..data_end] });
    }
    if at != cd_end {
        return Err(CrxError::BadZip("central directory size mismatch"));
    }

    // No two entries may share bytes (overlapping entries are the classic
    // way to make a small zip unpack to far more than it looks).
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(CrxError::BadZip("overlapping entries"));
    }
    check_unique(&entries)?;
    Ok(entries)
}

/// The end-of-central-directory record: the last one whose comment length
/// reaches exactly to the end of the data.
fn find_eocd(zip: &[u8]) -> Option<usize> {
    if zip.len() < EOCD_LEN {
        return None;
    }
    let lowest = zip.len().saturating_sub(EOCD_LEN + 0xffff);
    (lowest..=zip.len() - EOCD_LEN)
        .rev()
        .find(|&i| le32(zip, i) == EOCD_SIG && i + EOCD_LEN + usize::from(le16(zip, i + 20)) == zip.len())
}

/// Symlinks, devices and Windows reparse points are refused; only plain
/// files and folders are unpacked.
fn is_link_or_special(made_by_os: u8, attrs: u32) -> bool {
    const UNIX: u8 = 3;
    const S_IFMT: u32 = 0o170_000;
    const S_IFREG: u32 = 0o100_000;
    const S_IFDIR: u32 = 0o040_000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let kind = (attrs >> 16) & S_IFMT;
    (made_by_os == UNIX && kind != 0 && kind != S_IFREG && kind != S_IFDIR)
        || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// A zip entry name as a safe relative path: `(path, is_folder)`.
fn safe_rel_path(raw: &[u8]) -> Result<(String, bool), &'static str> {
    if raw.is_empty() || raw.len() > MAX_NAME_BYTES {
        return Err("empty or too long");
    }
    let name = std::str::from_utf8(raw).map_err(|_| "not UTF-8")?;
    if name.chars().any(|c| c.is_control() || c == '\\') {
        return Err("control character or backslash");
    }
    let (body, is_dir) = match name.strip_suffix('/') {
        Some(b) => (b, true),
        None => (name, false),
    };
    if body.is_empty() || body.starts_with('/') {
        return Err("absolute path");
    }
    let mut depth = 0;
    for part in body.split('/') {
        depth += 1;
        if depth > MAX_DEPTH {
            return Err("nested too deep");
        }
        if part.is_empty() || part == "." || part == ".." {
            return Err("empty, . or .. component");
        }
        if part.contains(['<', '>', ':', '"', '|', '?', '*']) {
            return Err("character Windows does not allow (drive, stream or wildcard)");
        }
        if part.ends_with(['.', ' ']) {
            return Err("component ends with a dot or space");
        }
        if is_reserved_name(part) {
            return Err("reserved device name");
        }
        if is_short_name_alias(part) {
            return Err("8.3 short-name alias");
        }
    }
    Ok((body.to_owned(), is_dir))
}

/// CON, PRN, AUX, NUL, COM0–9, LPT0–9 (and the superscript-digit forms),
/// CONIN$, CONOUT$, with or without an extension.
fn is_reserved_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && rest.len() == 1
        && (rest[0].is_ascii_digit() || matches!(rest[0], '¹' | '²' | '³'))
}

/// Looks like an 8.3 short name (`PROGRA~1`, `abc~12.js`): 1–6 characters
/// without `~`, then `~` and a digit. Such a name can open a different,
/// long-named file in the same folder, so it is refused.
fn is_short_name_alias(part: &str) -> bool {
    let mut chars = part.chars();
    let head = chars.by_ref().take_while(|&c| c != '~').count();
    (1..=6).contains(&head)
        && part.chars().nth(head) == Some('~')
        && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// Windows folders are case-insensitive: two names that differ only in case,
/// or a file where another entry needs a folder, are refused.
fn check_unique(entries: &[ZipEntry<'_>]) -> Result<(), CrxError> {
    let mut seen = HashSet::new();
    let mut files = HashSet::new();
    let mut dirs = HashSet::new();
    for e in entries {
        let key = e.name.to_lowercase();
        if !seen.insert(key.clone()) {
            return Err(CrxError::BadPath("duplicate name"));
        }
        let mut parent = key.as_str();
        while let Some((p, _)) = parent.rsplit_once('/') {
            dirs.insert(p.to_owned());
            parent = p;
        }
        if e.is_dir {
            dirs.insert(key);
        } else {
            files.insert(key);
        }
    }
    if files.iter().any(|f| dirs.contains(f)) {
        return Err(CrxError::BadPath("file and folder with the same name"));
    }
    Ok(())
}

/// The bytes of a file entry, checked against its declared size and CRC.
fn entry_bytes<'a>(e: &ZipEntry<'a>) -> Result<std::borrow::Cow<'a, [u8]>, CrxError> {
    let bytes = match e.method {
        0 => std::borrow::Cow::Borrowed(e.data),
        _ => std::borrow::Cow::Owned(
            // The limit is the declared size: a stream that inflates to more
            // stops there instead of filling memory.
            miniz_oxide::inflate::decompress_to_vec_with_limit(e.data, e.size as usize)
                .map_err(|_| CrxError::BadZip("bad deflate data or larger than declared"))?,
        ),
    };
    if bytes.len() as u64 != e.size {
        return Err(CrxError::BadZip("size differs from the declared size"));
    }
    if crc32(&bytes) != e.crc {
        return Err(CrxError::BadZip("CRC mismatch"));
    }
    Ok(bytes)
}

/// The package's `manifest.json` with `"key"` set to the developer's public
/// key, so the unpacked extension gets the id that was verified and not one
/// from a key the manifest brought along. Returns the entry's index and the
/// new bytes.
fn manifest_with_key(entries: &[ZipEntry<'_>], key: &[u8]) -> Result<(usize, Vec<u8>), CrxError> {
    let Some(i) = entries.iter().position(|e| !e.is_dir && e.name.eq_ignore_ascii_case("manifest.json")) else {
        return Err(CrxError::BadManifest("missing"));
    };
    let bytes = entry_bytes(&entries[i])?;
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(text).map_err(|_| CrxError::BadManifest("not valid JSON"))?;
    let Some(fields) = manifest.as_object_mut() else {
        return Err(CrxError::BadManifest("not a JSON object"));
    };
    fields.insert("key".to_owned(), base64(key).into());
    let out = serde_json::to_vec_pretty(&manifest).map_err(|_| CrxError::BadManifest("not writable"))?;
    Ok((i, out))
}

/// Standard base64 with padding (what a manifest's `"key"` holds).
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (k, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if k <= chunk.len() { char::from(ABC[(n >> shift & 63) as usize]) } else { '=' });
        }
    }
    out
}

/// Unpacks a verified package into `dest`, which must not exist or be an
/// empty folder. All of it or none of it: files go to a temp folder next to
/// `dest` that is renamed into place at the end and removed on any failure.
/// `manifest.json` gets the developer key as its `"key"`. Returns the number
/// of files written.
pub fn unpack(payload: &ZipPayload<'_>, dest: &Path) -> Result<usize, CrxError> {
    let entries = read_zip(payload.zip)?;
    let manifest = manifest_with_key(&entries, payload.developer_key)?;
    let dest_exists = match fs::symlink_metadata(dest) {
        Ok(m) => {
            if !m.is_dir() || m.file_type().is_symlink() || fs::read_dir(dest)?.next().is_some() {
                return Err(CrxError::DestNotEmpty);
            }
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    let (Some(parent), Some(leaf)) = (dest.parent(), dest.file_name()) else {
        return Err(CrxError::Io(io::Error::new(io::ErrorKind::InvalidInput, "no parent folder")));
    };
    let tmp = make_temp_dir(parent, &leaf.to_string_lossy())?;
    let result = write_entries(&entries, &tmp, &manifest).and_then(|n| {
        if dest_exists {
            fs::remove_dir(dest)?;
        }
        fs::rename(&tmp, dest)?;
        Ok(n)
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

fn make_temp_dir(parent: &Path, leaf: &str) -> Result<PathBuf, CrxError> {
    for n in 0..100 {
        let tmp = parent.join(format!(".{leaf}.unpack-{}-{n}", std::process::id()));
        match fs::create_dir(&tmp) {
            Ok(()) => return Ok(tmp),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(CrxError::Io(io::Error::new(io::ErrorKind::AlreadyExists, "no free temp folder name")))
}

/// Writes every entry under `root`; entry `replace.0` gets `replace.1`
/// instead of its own bytes.
fn write_entries(entries: &[ZipEntry<'_>], root: &Path, replace: &(usize, Vec<u8>)) -> Result<usize, CrxError> {
    let mut files = 0;
    for (i, e) in entries.iter().enumerate() {
        let mut path = root.to_path_buf();
        path.extend(e.name.split('/'));
        if e.is_dir {
            fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let bytes = if i == replace.0 { std::borrow::Cow::Borrowed(&replace.1[..]) } else { entry_bytes(e)? };
        // create_new: a second name that the file system folds onto an
        // existing file (beyond what `check_unique` catches) fails here.
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        f.write_all(&bytes)?;
        files += 1;
    }
    Ok(files)
}

/// CRC-32 (IEEE), as zip uses it.
fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    !data.iter().fold(!0u32, |c, &b| TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8))
}

#[cfg(any(test, feature = "selftest"))]
pub(crate) mod testpkg;
#[cfg(test)]
mod tests;
