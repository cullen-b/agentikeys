//! End-to-end encryption for values sent through a tunnel: ECDH P-256 -> HKDF-SHA256 -> AES-256-GCM.
//! The browser side lives in form.html and must stay in step with this.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::{PublicKey, SecretKey};
use serde::Deserialize;

const INFO: &[u8] = b"agentikeys v1";

pub struct Opener {
    secret: SecretKey,
}

/// What the browser sends: its ephemeral public key, the nonce and the ciphertext, all base64url.
#[derive(Deserialize)]
pub struct Sealed {
    epk: String,
    iv: String,
    ct: String,
}

impl Opener {
    pub fn new() -> Result<Opener> {
        loop {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|e| anyhow!("no randomness: {e}"))?;
            if let Ok(secret) = SecretKey::from_slice(&bytes) {
                return Ok(Opener { secret });
            }
        }
    }

    /// Uncompressed SEC1 public key, base64url; goes in the link's #fragment.
    pub fn public_key(&self) -> String {
        B64.encode(self.secret.public_key().to_sec1_bytes())
    }

    /// Decrypts a submission; `aad` binds it to this request.
    pub fn open(&self, sealed: &Sealed, aad: &[u8]) -> Result<Vec<u8>> {
        let bad = || anyhow!("couldn't decrypt the submission");
        let epk = PublicKey::from_sec1_bytes(&B64.decode(&sealed.epk).map_err(|_| bad())?).map_err(|_| bad())?;
        let iv = B64.decode(&sealed.iv).map_err(|_| bad())?;
        let ct = B64.decode(&sealed.ct).map_err(|_| bad())?;
        let shared = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), epk.as_affine());
        let mut key = [0u8; 32];
        shared.extract::<sha2::Sha256>(None).expand(INFO, &mut key).map_err(|_| bad())?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| bad())?;
        let nonce = Nonce::try_from(iv.as_slice()).map_err(|_| bad())?;
        cipher.decrypt(&nonce, Payload { msg: &ct, aad }).map_err(|_| bad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encrypts the way form.html does, to check the two sides agree.
    fn seal(recipient_b64: &str, plaintext: &[u8], aad: &[u8]) -> Sealed {
        let recipient = PublicKey::from_sec1_bytes(&B64.decode(recipient_b64).unwrap()).unwrap();
        let eph = Opener::new().unwrap();
        let shared = p256::ecdh::diffie_hellman(eph.secret.to_nonzero_scalar(), recipient.as_affine());
        let mut key = [0u8; 32];
        shared.extract::<sha2::Sha256>(None).expand(INFO, &mut key).unwrap();
        let iv = [7u8; 12];
        let nonce = Nonce::try_from(&iv[..]).unwrap();
        let ct = Aes256Gcm::new_from_slice(&key).unwrap().encrypt(&nonce, Payload { msg: plaintext, aad }).unwrap();
        Sealed { epk: eph.public_key(), iv: B64.encode(iv), ct: B64.encode(ct) }
    }

    #[test]
    fn round_trip_and_aad_binding() {
        let opener = Opener::new().unwrap();
        let sealed = seal(&opener.public_key(), b"{\"values\":{}}", b"token-a");
        assert_eq!(opener.open(&sealed, b"token-a").unwrap(), b"{\"values\":{}}");
        assert!(opener.open(&sealed, b"token-b").is_err());
        assert!(Opener::new().unwrap().open(&sealed, b"token-a").is_err());
    }
}
