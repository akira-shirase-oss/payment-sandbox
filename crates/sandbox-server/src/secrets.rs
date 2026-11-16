//! Webhook signing secrets at rest (D-020): ChaCha20-Poly1305 under the server key
//! (`PS_SECRET_KEY`, 32 bytes as 64 hex characters), with the endpoint's project and id as
//! associated data, so a ciphertext copied onto another row does not decrypt. Signing
//! secrets cannot be hashed: the server needs them to compute signatures.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use sandbox_db::webhooks::Sealed;
use uuid::Uuid;

/// The server's secret box.
pub struct SecretBox {
    cipher: ChaCha20Poly1305,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(<key redacted>)")
    }
}

/// A sealing or opening failure (wrong key, tampered row, no randomness).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("secret could not be sealed or opened")]
pub struct SealError;

fn associated(project: Uuid, endpoint: Uuid) -> [u8; 32] {
    let mut aad = [0_u8; 32];
    aad[..16].copy_from_slice(project.as_bytes());
    aad[16..].copy_from_slice(endpoint.as_bytes());
    aad
}

impl SecretBox {
    /// Builds the box from 64 hex characters.
    pub fn from_hex(key: &str) -> Result<Self, SealError> {
        let bytes = hex::decode(key).map_err(|_| SealError)?;
        let cipher = ChaCha20Poly1305::new_from_slice(&bytes).map_err(|_| SealError)?;
        Ok(Self { cipher })
    }

    /// Encrypts `secret` for the endpoint `(project, endpoint)`.
    pub fn seal(&self, secret: &str, project: Uuid, endpoint: Uuid) -> Result<Sealed, SealError> {
        let mut nonce = [0_u8; 12];
        getrandom::fill(&mut nonce).map_err(|_| SealError)?;
        let aad = associated(project, endpoint);
        let ciphertext = self
            .cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| SealError)?;
        Ok(Sealed {
            ciphertext,
            nonce: nonce.to_vec(),
        })
    }

    /// Decrypts a sealed secret of the endpoint `(project, endpoint)`.
    pub fn open(
        &self,
        sealed: &Sealed,
        project: Uuid,
        endpoint: Uuid,
    ) -> Result<String, SealError> {
        let nonce: [u8; 12] = sealed.nonce.as_slice().try_into().map_err(|_| SealError)?;
        let aad = associated(project, endpoint);
        let plain = self
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &sealed.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError)?;
        String::from_utf8(plain).map_err(|_| SealError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn a_sealed_secret_opens_only_for_its_own_row_and_key() {
        let sb = SecretBox::from_hex(KEY).unwrap();
        let (p, e) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let sealed = sb.seal("whsec_sim_abc", p, e).unwrap();
        assert!(
            !sealed.ciphertext.windows(5).any(|w| w == b"whsec"),
            "ciphertext carries plaintext"
        );
        assert_eq!(sb.open(&sealed, p, e).unwrap(), "whsec_sim_abc");
        assert_eq!(
            sb.open(&sealed, p, Uuid::from_u128(3)),
            Err(SealError),
            "moved to another row"
        );
        let other = SecretBox::from_hex(&KEY.replace("00", "ff")).unwrap();
        assert_eq!(other.open(&sealed, p, e), Err(SealError), "another key");
        let mut tampered = sealed.clone();
        tampered.ciphertext[0] ^= 1;
        assert_eq!(sb.open(&tampered, p, e), Err(SealError));
        assert!(SecretBox::from_hex("short").is_err());
    }
}
