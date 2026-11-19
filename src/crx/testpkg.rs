//! Signed test packages: throwaway CNG keys, CRX3 headers and zips. Built for
//! the unit tests and for the self-test build, which installs fake Web Store
//! packages; a normal build compiles none of it.

#![cfg_attr(not(test), allow(dead_code))]

use super::*;
use core::ffi::c_void;
use windows::Win32::Security::Cryptography::{
    BCRYPT_ECDSA_P256_ALG_HANDLE, BCRYPT_FLAGS, BCRYPT_KEY_HANDLE, BCRYPT_PAD_PKCS1,
    BCRYPT_PKCS1_PADDING_INFO, BCRYPT_RSA_ALG_HANDLE, BCRYPT_SHA256_ALGORITHM, BCryptDestroyKey,
    BCryptFinalizeKeyPair, BCryptGenerateKeyPair, BCryptSignHash, CRYPT_ENCODE_OBJECT_FLAGS,
    CryptEncodeObjectEx, CryptExportPublicKeyInfoFromBCryptKeyHandle, X509_ASN_ENCODING,
    X509_PUBLIC_KEY_INFO, szOID_ECC_PUBLIC_KEY, szOID_RSA_RSA,
};

// --- keys ------------------------------------------------------------------

pub(crate) struct TestKey {
    h: BCRYPT_KEY_HANDLE,
    alg: Alg,
    pub(crate) spki: Vec<u8>,
}

impl Drop for TestKey {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.h);
        }
    }
}

impl TestKey {
    pub(crate) fn rsa() -> Self {
        Self::generate(Alg::Rsa, 2048)
    }

    pub(crate) fn p256() -> Self {
        Self::generate(Alg::EcdsaP256, 256)
    }

    pub(crate) fn rsa1024() -> Self {
        Self::generate(Alg::Rsa, 1024)
    }

    fn generate(alg: Alg, bits: u32) -> Self {
        unsafe {
            let (handle, oid) = match alg {
                Alg::Rsa => (BCRYPT_RSA_ALG_HANDLE, szOID_RSA_RSA),
                Alg::EcdsaP256 => (BCRYPT_ECDSA_P256_ALG_HANDLE, szOID_ECC_PUBLIC_KEY),
            };
            let mut h = BCRYPT_KEY_HANDLE::default();
            BCryptGenerateKeyPair(handle, &mut h, bits, 0).ok().unwrap();
            BCryptFinalizeKeyPair(h, 0).ok().unwrap();

            let mut len = 0u32;
            assert!(
                CryptExportPublicKeyInfoFromBCryptKeyHandle(h, X509_ASN_ENCODING, oid, 0, None, None, &mut len)
                    .as_bool()
            );
            let mut info = vec![0u64; (len as usize).div_ceil(8)];
            assert!(
                CryptExportPublicKeyInfoFromBCryptKeyHandle(
                    h,
                    X509_ASN_ENCODING,
                    oid,
                    0,
                    None,
                    Some(info.as_mut_ptr().cast()),
                    &mut len,
                )
                .as_bool()
            );
            let mut der_len = 0u32;
            CryptEncodeObjectEx(
                X509_ASN_ENCODING,
                X509_PUBLIC_KEY_INFO,
                info.as_ptr().cast(),
                CRYPT_ENCODE_OBJECT_FLAGS(0),
                None,
                None,
                &mut der_len,
            )
            .unwrap();
            let mut spki = vec![0u8; der_len as usize];
            CryptEncodeObjectEx(
                X509_ASN_ENCODING,
                X509_PUBLIC_KEY_INFO,
                info.as_ptr().cast(),
                CRYPT_ENCODE_OBJECT_FLAGS(0),
                None,
                Some(spki.as_mut_ptr().cast()),
                &mut der_len,
            )
            .unwrap();
            spki.truncate(der_len as usize);
            Self { h, alg, spki }
        }
    }

    /// The 16-byte id prefix of this key's SHA-256.
    pub(crate) fn crx_id(&self) -> [u8; 16] {
        cng::sha256(&[&self.spki]).unwrap()[..16].try_into().unwrap()
    }

    pub(crate) fn id(&self) -> String {
        id_from_hash(&self.crx_id())
    }

    /// The CRX signature (DER for ECDSA, as CRX carries it).
    pub(crate) fn sign(&self, digest: &[u8; 32]) -> Vec<u8> {
        unsafe {
            let pad = BCRYPT_PKCS1_PADDING_INFO { pszAlgId: BCRYPT_SHA256_ALGORITHM };
            let (info, flags) = match self.alg {
                Alg::Rsa => (Some((&raw const pad).cast::<c_void>()), BCRYPT_PAD_PKCS1),
                Alg::EcdsaP256 => (None, BCRYPT_FLAGS(0)),
            };
            let mut len = 0u32;
            BCryptSignHash(self.h, info, digest, None, &mut len, flags).ok().unwrap();
            let mut sig = vec![0u8; len as usize];
            BCryptSignHash(self.h, info, digest, Some(&mut sig), &mut len, flags).ok().unwrap();
            sig.truncate(len as usize);
            match self.alg {
                Alg::Rsa => sig,
                Alg::EcdsaP256 => raw_to_der(&sig),
            }
        }
    }

    pub(crate) fn slot(&self) -> u64 {
        match self.alg {
            Alg::Rsa => 2,
            Alg::EcdsaP256 => 3,
        }
    }
}

/// `r || s` to a DER `ECDSA-Sig-Value`.
pub(crate) fn raw_to_der(raw: &[u8]) -> Vec<u8> {
    let int = |v: &[u8]| {
        let mut v = v.iter().skip_while(|&&b| b == 0).copied().collect::<Vec<_>>();
        if v.is_empty() || v[0] & 0x80 != 0 {
            v.insert(0, 0);
        }
        let mut out = vec![0x02, v.len() as u8];
        out.extend(v);
        out
    };
    let (r, s) = raw.split_at(raw.len() / 2);
    let body = [int(r), int(s)].concat();
    let mut out = vec![0x30, body.len() as u8];
    out.extend(body);
    out
}

// --- CRX building ----------------------------------------------------------

pub(crate) fn pb_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn pb_bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    pb_varint(out, field << 3 | 2);
    pb_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

pub(crate) fn signed_data(crx_id: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    pb_bytes(&mut out, 1, crx_id);
    out
}

pub(crate) fn digest(signed: &[u8], zip: &[u8]) -> [u8; 32] {
    cng::sha256(&[SIGNED_CONTEXT, &(signed.len() as u32).to_le_bytes(), signed, zip]).unwrap()
}

pub(crate) fn proof(key: &[u8], sig: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    pb_bytes(&mut out, 1, key);
    pb_bytes(&mut out, 2, sig);
    out
}

/// A header with these `(field, proof)` pairs and the signed data.
pub(crate) fn header(proofs: &[(u64, Vec<u8>)], signed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (field, p) in proofs {
        pb_bytes(&mut out, *field, p);
    }
    pb_bytes(&mut out, 10000, signed);
    out
}

pub(crate) fn assemble(header: &[u8], zip: &[u8]) -> Vec<u8> {
    let mut out = b"Cr24".to_vec();
    out.extend(3u32.to_le_bytes());
    out.extend((header.len() as u32).to_le_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(zip);
    out
}

/// A package with `crx_id`, signed by each key.
pub(crate) fn crx_signed(zip: &[u8], crx_id: &[u8; 16], keys: &[&TestKey]) -> Vec<u8> {
    let signed = signed_data(crx_id);
    let d = digest(&signed, zip);
    let proofs: Vec<_> = keys.iter().map(|k| (k.slot(), proof(&k.spki, &k.sign(&d)))).collect();
    assemble(&header(&proofs, &signed), zip)
}

/// A package for `keys[0]`'s id, signed by every key.
pub(crate) fn crx(zip: &[u8], keys: &[&TestKey]) -> Vec<u8> {
    crx_signed(zip, &keys[0].crx_id(), keys)
}

// --- zip building ----------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Ent {
    pub(crate) name: Vec<u8>,
    pub(crate) data: Vec<u8>,
    pub(crate) method: u16,
    pub(crate) size: u32,
    pub(crate) crc: u32,
    pub(crate) made_by: u16,
    pub(crate) attrs: u32,
    pub(crate) flags: u16,
    /// Compressed size written in the central directory, if not `data.len()`.
    pub(crate) csize: Option<u32>,
}

pub(crate) fn stored(name: &str, data: &[u8]) -> Ent {
    Ent {
        name: name.as_bytes().to_vec(),
        data: data.to_vec(),
        method: 0,
        size: data.len() as u32,
        crc: crc32(data),
        made_by: 20,
        attrs: 0,
        flags: 0,
        csize: None,
    }
}

pub(crate) fn deflated(name: &str, data: &[u8]) -> Ent {
    Ent { data: miniz_oxide::deflate::compress_to_vec(data, 6), method: 8, ..stored(name, data) }
}

pub(crate) fn dir(name: &str) -> Ent {
    Ent { attrs: 0x10, ..stored(name, b"") }
}

pub(crate) fn zip(ents: &[Ent]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in ents {
        let offset = out.len() as u32;
        let csize = e.csize.unwrap_or(e.data.len() as u32);
        out.extend(LOCAL_SIG.to_le_bytes());
        out.extend(20u16.to_le_bytes());
        out.extend(e.flags.to_le_bytes());
        out.extend(e.method.to_le_bytes());
        out.extend([0u8; 4]); // time, date
        out.extend(e.crc.to_le_bytes());
        out.extend((e.data.len() as u32).to_le_bytes());
        out.extend(e.size.to_le_bytes());
        out.extend((e.name.len() as u16).to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(&e.name);
        out.extend(&e.data);

        central.extend(CENTRAL_SIG.to_le_bytes());
        central.extend(e.made_by.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(e.flags.to_le_bytes());
        central.extend(e.method.to_le_bytes());
        central.extend([0u8; 4]);
        central.extend(e.crc.to_le_bytes());
        central.extend(csize.to_le_bytes());
        central.extend(e.size.to_le_bytes());
        central.extend((e.name.len() as u16).to_le_bytes());
        central.extend([0u8; 4]); // extra, comment lengths
        central.extend([0u8; 4]); // disk, internal attributes
        central.extend(e.attrs.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(&e.name);
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(EOCD_SIG.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((ents.len() as u16).to_le_bytes());
    out.extend((ents.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}
