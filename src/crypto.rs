//! Envelope encryption (R2/S3-SSE style) plus HMAC helpers.
//!
//! * Every blob gets its own random 256-bit data key (DEK).
//! * The DEK is wrapped with a versioned key-encryption key (KEK) derived from
//!   `TGV_MASTER_KEYS`. Rotating keys only re-wraps DEKs; no chunk is re-uploaded.
//! * Chunk bytes are sealed in 64 KiB AES-256-GCM blocks. The nonce is
//!   `chunk_salt(8 random bytes) || block_index(u32)` and the AAD binds each block to
//!   its blob, chunk and position, so blocks cannot be reordered or swapped between
//!   chunks or files. Fixed-size blocks make byte ranges seekable: serving a video
//!   seek only decrypts the blocks it touches.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Result};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;
use std::collections::BTreeMap;
use subtle::ConstantTimeEq;

pub const BLOCK: usize = 64 * 1024;
pub const TAG: usize = 16;
pub const SEALED_BLOCK: usize = BLOCK + TAG;

type HmacSha256 = Hmac<Sha256>;

fn derive(ikm: &[u8; 32], info: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(b"tgvault/v1"), ikm);
    let mut out = [0u8; 32];
    hk.expand(info.as_bytes(), &mut out).expect("32 bytes is a valid HKDF length");
    out
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Unbiased base62 token (rejection sampling).
pub fn random_token(len: usize) -> String {
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        OsRng.fill_bytes(&mut buf);
        for b in buf {
            if b < 248 && out.len() < len {
                out.push(ALPHABET[(b % 62) as usize] as char);
            }
        }
    }
    out
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

pub struct Keyring {
    keks: BTreeMap<u32, Aes256Gcm>,
    pub active: u32,
    pepper: [u8; 32],
    presign: [u8; 32],
    share: [u8; 32],
}

impl Keyring {
    pub fn new(master: &BTreeMap<u32, [u8; 32]>, active: u32, server_secret: &[u8; 32]) -> Self {
        let keks = master
            .iter()
            .map(|(v, k)| (*v, Aes256Gcm::new(&derive(k, "kek").into())))
            .collect();
        Self {
            keks,
            active,
            pepper: derive(server_secret, "api-key-pepper"),
            presign: derive(server_secret, "presign"),
            share: derive(server_secret, "share-ticket"),
        }
    }

    pub fn versions(&self) -> Vec<u32> {
        self.keks.keys().copied().collect()
    }

    fn kek(&self, version: u32) -> Result<&Aes256Gcm> {
        self.keks.get(&version).ok_or_else(|| anyhow!("master key version {version} is not configured"))
    }

    pub fn new_dek() -> [u8; 32] {
        random_bytes()
    }

    /// Returns `nonce || ciphertext` and the KEK version used.
    pub fn wrap_dek(&self, dek: &[u8; 32], blob_id: &str) -> Result<(Vec<u8>, u32)> {
        let nonce: [u8; 12] = random_bytes();
        let aad = format!("dek:{blob_id}");
        let ct = self
            .kek(self.active)?
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: dek, aad: aad.as_bytes() })
            .map_err(|_| anyhow!("wrap failed"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        Ok((out, self.active))
    }

    pub fn unwrap_dek(&self, wrapped: &[u8], version: u32, blob_id: &str) -> Result<[u8; 32]> {
        if wrapped.len() != 12 + 32 + TAG {
            bail!("wrapped key has the wrong length");
        }
        let aad = format!("dek:{blob_id}");
        let pt = self
            .kek(version)?
            .decrypt(Nonce::from_slice(&wrapped[..12]), Payload { msg: &wrapped[12..], aad: aad.as_bytes() })
            .map_err(|_| anyhow!("data key failed authentication (wrong master key?)"))?;
        pt.try_into().map_err(|_| anyhow!("bad data key length"))
    }

    /// Seal an arbitrary small payload (metadata backups) with the active KEK.
    /// Layout: b"TGVB" || version u32 BE || nonce(12) || ciphertext.
    pub fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let nonce: [u8; 12] = random_bytes();
        let ct = self
            .kek(self.active)?
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| anyhow!("seal failed"))?;
        let mut out = Vec::with_capacity(20 + ct.len());
        out.extend_from_slice(b"TGVB");
        out.extend_from_slice(&self.active.to_be_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 20 + TAG || &sealed[..4] != b"TGVB" {
            bail!("not a tgvault sealed file");
        }
        let version = u32::from_be_bytes(sealed[4..8].try_into().unwrap());
        self.kek(version)?
            .decrypt(Nonce::from_slice(&sealed[8..20]), Payload { msg: &sealed[20..], aad })
            .map_err(|_| anyhow!("sealed file failed authentication"))
    }

    fn mac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
        let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac accepts any key length");
        for p in parts {
            // Length-prefix every part so ("ab","c") and ("a","bc") never collide.
            m.update(&(p.len() as u64).to_be_bytes());
            m.update(p);
        }
        m.finalize().into_bytes().into()
    }

    /// Stored form of an API-key secret or a share token. Secrets are 190+ bits of
    /// entropy, so a peppered HMAC is enough; a DB leak alone does not reveal keys.
    pub fn hash_secret(&self, secret: &str) -> String {
        hex::encode(Self::mac(&self.pepper, &[secret.as_bytes()]))
    }

    pub fn presign_sig(&self, parts: &[&[u8]]) -> String {
        hex::encode(Self::mac(&self.presign, parts))
    }

    pub fn share_sig(&self, parts: &[&[u8]]) -> String {
        hex::encode(Self::mac(&self.share, parts))
    }
}

pub struct ChunkCipher {
    aead: Aes256Gcm,
    blob_id: String,
}

impl ChunkCipher {
    pub fn new(dek: &[u8; 32], blob_id: &str) -> Self {
        Self { aead: Aes256Gcm::new(dek.into()), blob_id: blob_id.to_string() }
    }

    fn nonce_aad(&self, salt: &[u8; 8], idx: u32) -> ([u8; 12], Vec<u8>) {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(salt);
        nonce[8..].copy_from_slice(&idx.to_be_bytes());
        let mut aad = Vec::with_capacity(self.blob_id.len() + 12);
        aad.extend_from_slice(self.blob_id.as_bytes());
        aad.extend_from_slice(salt);
        aad.extend_from_slice(&idx.to_be_bytes());
        (nonce, aad)
    }

    pub fn sealed_len(plain_len: usize) -> usize {
        plain_len + plain_len.div_ceil(BLOCK).max(1) * TAG
    }

    pub fn encrypt_chunk(&self, salt: &[u8; 8], plain: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(Self::sealed_len(plain.len()));
        // An empty chunk still gets one (empty) authenticated block.
        let blocks: Vec<&[u8]> = if plain.is_empty() { vec![&[][..]] } else { plain.chunks(BLOCK).collect() };
        for (i, block) in blocks.into_iter().enumerate() {
            let (nonce, aad) = self.nonce_aad(salt, i as u32);
            let ct = self
                .aead
                .encrypt(Nonce::from_slice(&nonce), Payload { msg: block, aad: &aad })
                .map_err(|_| anyhow!("encrypt failed"))?;
            out.extend_from_slice(&ct);
        }
        Ok(out)
    }

    /// Decrypt plaintext bytes `[from, to)` of a chunk whose plaintext is `plain_len` long.
    pub fn decrypt_range(&self, salt: &[u8; 8], sealed: &[u8], plain_len: usize, from: usize, to: usize) -> Result<Vec<u8>> {
        if from > to || to > plain_len {
            bail!("range outside chunk");
        }
        if sealed.len() != Self::sealed_len(plain_len) {
            bail!("chunk has unexpected size {} (expected {})", sealed.len(), Self::sealed_len(plain_len));
        }
        let mut out = Vec::with_capacity(to - from);
        if from == to {
            return Ok(out);
        }
        let first = from / BLOCK;
        let last = (to - 1) / BLOCK;
        for i in first..=last {
            let start = i * SEALED_BLOCK;
            let end = (start + SEALED_BLOCK).min(sealed.len());
            let (nonce, aad) = self.nonce_aad(salt, i as u32);
            let pt = self
                .aead
                .decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed[start..end], aad: &aad })
                .map_err(|_| anyhow!("chunk block {i} failed authentication"))?;
            let block_start = i * BLOCK;
            let lo = from.saturating_sub(block_start);
            let hi = (to - block_start).min(pt.len());
            out.extend_from_slice(&pt[lo..hi]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ranges() {
        let dek = Keyring::new_dek();
        let c = ChunkCipher::new(&dek, "blob1");
        let salt: [u8; 8] = random_bytes();
        let plain: Vec<u8> = (0..(BLOCK * 3 + 123)).map(|i| (i % 251) as u8).collect();
        let sealed = c.encrypt_chunk(&salt, &plain).unwrap();
        assert_eq!(sealed.len(), ChunkCipher::sealed_len(plain.len()));
        for (a, b) in [(0, plain.len()), (5, 10), (BLOCK - 3, BLOCK + 3), (BLOCK * 3, plain.len()), (7, 7)] {
            assert_eq!(c.decrypt_range(&salt, &sealed, plain.len(), a, b).unwrap(), &plain[a..b]);
        }
    }

    #[test]
    fn tamper_and_swap_detected() {
        let dek = Keyring::new_dek();
        let c = ChunkCipher::new(&dek, "blob1");
        let salt: [u8; 8] = random_bytes();
        let plain = vec![7u8; BLOCK * 2];
        let mut sealed = c.encrypt_chunk(&salt, &plain).unwrap();
        sealed[10] ^= 1;
        assert!(c.decrypt_range(&salt, &sealed, plain.len(), 0, 5).is_err());
        // Same key, other blob id => AAD mismatch.
        let sealed = c.encrypt_chunk(&salt, &plain).unwrap();
        let other = ChunkCipher::new(&dek, "blob2");
        assert!(other.decrypt_range(&salt, &sealed, plain.len(), 0, 5).is_err());
        // Swapped blocks => position mismatch.
        let mut swapped = sealed[SEALED_BLOCK..].to_vec();
        swapped.extend_from_slice(&sealed[..SEALED_BLOCK]);
        assert!(c.decrypt_range(&salt, &swapped, plain.len(), 0, 5).is_err());
    }

    #[test]
    fn empty_chunk() {
        let c = ChunkCipher::new(&Keyring::new_dek(), "b");
        let salt = [0u8; 8];
        let sealed = c.encrypt_chunk(&salt, &[]).unwrap();
        assert_eq!(sealed.len(), TAG);
        assert!(c.decrypt_range(&salt, &sealed, 0, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn dek_wrap_rotation() {
        let mut m = BTreeMap::new();
        m.insert(1, random_bytes());
        m.insert(2, random_bytes());
        let k1 = Keyring::new(&m, 1, &random_bytes());
        let dek = Keyring::new_dek();
        let (w, v) = k1.wrap_dek(&dek, "b").unwrap();
        let k2 = Keyring::new(&m, 2, &random_bytes());
        assert_eq!(k2.unwrap_dek(&w, v, "b").unwrap(), dek);
        assert!(k2.unwrap_dek(&w, v, "other").is_err());
    }

    #[test]
    fn token_alphabet() {
        let t = random_token(40);
        assert_eq!(t.len(), 40);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()));
    }
}
