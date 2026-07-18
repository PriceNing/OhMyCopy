use crate::config::PROTOCOL_SALT;
use anyhow::{bail, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;

/// Cached root key derived from shared password.
#[derive(Clone)]
pub struct AuthMaterial {
    root_key: [u8; 32],
}

impl AuthMaterial {
    pub fn from_password(password: &str) -> Result<Self> {
        // Mild params for N100: still slow enough vs brute force offline.
        let params = Params::new(19 * 1024, 2, 1, Some(32))
            .map_err(|e| anyhow::anyhow!("argon2 params: {e}"))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut root_key = [0u8; 32];
        argon2
            .hash_password_into(password.as_bytes(), PROTOCOL_SALT, &mut root_key)
            .map_err(|e| anyhow::anyhow!("argon2 failed: {e}"))?;
        Ok(Self { root_key })
    }

    /// Proof = BLAKE3 keyed hash of nonce with root_key as key material.
    pub fn prove(&self, nonce: &[u8; 32]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_keyed(&self.root_key);
        hasher.update(b"ohmycopy-auth-v1");
        hasher.update(nonce);
        *hasher.finalize().as_bytes()
    }

    pub fn verify_proof(&self, nonce: &[u8; 32], proof: &[u8; 32]) -> bool {
        let expected = self.prove(nonce);
        expected
            .iter()
            .zip(proof.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    pub fn session_key(&self, client_nonce: &[u8; 32], server_nonce: &[u8; 32]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_keyed(&self.root_key);
        hasher.update(b"ohmycopy-session-v1");
        hasher.update(client_nonce);
        hasher.update(server_nonce);
        *hasher.finalize().as_bytes()
    }

    /// Directional keys so both peers can send without AEAD nonce collision.
    pub fn directional_keys(
        &self,
        client_nonce: &[u8; 32],
        server_nonce: &[u8; 32],
    ) -> ([u8; 32], [u8; 32]) {
        let base = self.session_key(client_nonce, server_nonce);
        let mut send_a = blake3::Hasher::new_keyed(&base);
        send_a.update(b"send-from-smaller-id");
        let mut send_b = blake3::Hasher::new_keyed(&base);
        send_b.update(b"send-from-larger-id");
        (*send_a.finalize().as_bytes(), *send_b.finalize().as_bytes())
    }
}

/// One-direction AEAD (each peer has encrypt key for its send path).
pub struct SessionCrypto {
    cipher: ChaCha20Poly1305,
    send_counter: u64,
}

impl SessionCrypto {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            send_counter: 0,
        }
    }

    fn nonce_from_counter(counter: u64) -> Nonce {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&counter.to_le_bytes());
        Nonce::from(n)
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.send_counter = self.send_counter.saturating_add(1);
        let counter = self.send_counter;
        let nonce = Self::nonce_from_counter(counter);
        let mut out = Vec::with_capacity(8 + plaintext.len() + 16);
        out.extend_from_slice(&counter.to_le_bytes());
        let ct = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn open(&self, data: &[u8], recv_max: &mut u64) -> Result<Vec<u8>> {
        if data.len() < 8 + 16 {
            bail!("ciphertext too short");
        }
        let mut cbytes = [0u8; 8];
        cbytes.copy_from_slice(&data[..8]);
        let counter = u64::from_le_bytes(cbytes);
        if counter <= *recv_max {
            bail!("replay or out-of-order counter");
        }
        let nonce = Self::nonce_from_counter(counter);
        let plain = self
            .cipher
            .decrypt(&nonce, &data[8..])
            .map_err(|_| anyhow::anyhow!("decrypt failed (bad password or corrupt)"))?;
        *recv_max = counter;
        Ok(plain)
    }
}

pub fn random_nonce() -> [u8; 32] {
    let mut n = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut n);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_crypto_roundtrip() {
        let a = AuthMaterial::from_password("family-secret").unwrap();
        let b = AuthMaterial::from_password("family-secret").unwrap();
        assert_eq!(a.root_key, b.root_key);

        let n1 = random_nonce();
        let n2 = random_nonce();
        let proof = a.prove(&n1);
        assert!(b.verify_proof(&n1, &proof));

        let (k_small, k_large) = a.directional_keys(&n1, &n2);
        // Smaller id peer encrypts with k_small; larger decrypts with k_small.
        let mut enc = SessionCrypto::new(&k_small);
        let dec = SessionCrypto::new(&k_small);
        let mut recv_max = 0u64;
        let ct = enc.seal(b"hello clipboard").unwrap();
        let pt = dec.open(&ct, &mut recv_max).unwrap();
        assert_eq!(pt, b"hello clipboard");

        // Other direction
        let mut enc2 = SessionCrypto::new(&k_large);
        let dec2 = SessionCrypto::new(&k_large);
        let mut recv_max2 = 0u64;
        let ct2 = enc2.seal(b"pong").unwrap();
        assert_eq!(dec2.open(&ct2, &mut recv_max2).unwrap(), b"pong");
    }

    #[test]
    fn wrong_password_differs() {
        let a = AuthMaterial::from_password("a").unwrap();
        let b = AuthMaterial::from_password("b").unwrap();
        assert_ne!(a.root_key, b.root_key);
    }
}
