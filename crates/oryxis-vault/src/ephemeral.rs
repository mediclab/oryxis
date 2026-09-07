//! A key that exists only inside one process, for data that must not
//! reach the disk in the clear while the vault holds no key of its own.
//!
//! The soft lock zeroizes the master key and keeps the live sessions;
//! anything they produce meanwhile (a recording, above all) can neither
//! be sealed with the vault's key nor sit in memory without bound. This
//! key fills that window: random, never written anywhere, gone with the
//! process. A file sealed under it is exactly as readable at rest as the
//! process's own memory, which is the posture the lock already accepts,
//! and unreadable to the next launch, whose job is to delete it.
//!
//! The format is the vault's own derived-key field format
//! (`encrypt_with_key`), so there is one AEAD construction in the crate,
//! not two.

use zeroize::Zeroize;

use crate::store::{decrypt_with_key, encrypt_with_key, os_random, VaultError};

/// 256 bits from the OS RNG, zeroized on drop.
pub struct EphemeralKey([u8; 32]);

impl EphemeralKey {
    pub fn generate() -> Result<Self, VaultError> {
        let mut key = [0u8; 32];
        os_random(&mut key)?;
        Ok(Self(key))
    }

    /// Seal `plaintext`: tag + random nonce + ciphertext, the vault's
    /// derived-key field layout.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        encrypt_with_key(plaintext, &self.0)
    }

    /// Inverse of [`Self::seal`]. A blob sealed under another key, or
    /// touched since, fails rather than decrypting to noise.
    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>, VaultError> {
        decrypt_with_key(blob, &self.0)
    }
}

impl Drop for EphemeralKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for EphemeralKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EphemeralKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::EphemeralKey;

    #[test]
    fn a_sealed_blob_opens_under_its_own_key_only() {
        let key = EphemeralKey::generate().expect("rng");
        let other = EphemeralKey::generate().expect("rng");
        let blob = key.seal(b"recorded while locked").expect("seal");
        assert_ne!(&blob[..], b"recorded while locked");
        assert_eq!(key.open(&blob).expect("open"), b"recorded while locked");
        assert!(other.open(&blob).is_err(), "another process's key read the blob");
    }

    #[test]
    fn a_touched_blob_is_refused() {
        let key = EphemeralKey::generate().expect("rng");
        let mut blob = key.seal(b"payload").expect("seal");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(key.open(&blob).is_err());
    }

    #[test]
    fn two_seals_of_one_payload_differ() {
        let key = EphemeralKey::generate().expect("rng");
        let a = key.seal(b"same").expect("seal");
        let b = key.seal(b"same").expect("seal");
        assert_ne!(a, b, "a repeated nonce");
    }
}
