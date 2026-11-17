//! ed25519 index signing and verification (M2b).
//!
//! A binary repo ships `index.json` alongside `index.json.sig`
//! (ed25519). The public key is embedded in gozpak (spec v1.2); the
//! private signing key lives only on the packaging host.
//!
//! On-disk formats:
//!
//! - `index.json.sig` — the 64-byte ed25519 signature as 128 lowercase
//!   hex characters (one trailing newline tolerated on read).
//! - signing key (private) — the 32-byte seed as 64 lowercase hex
//!   characters, stored `0600` on the packaging host.
//!
//! The signature covers the exact bytes of `index.json` as shipped —
//! never a normalized/re-serialized form.

use ed25519_dalek::ed25519;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

/// The GozJaro repository public key, embedded in gozpak (hex, 64
/// chars). Verification of every `index.json` runs against this key
/// (or an override — see [`verify_with`] in the CLI layer).
///
/// The matching private key is held only by the packaging host
/// (7D12, `/home/weepingdogel/.secrets/gozjaro-repo.key`, 0600); it is
/// never committed to any repository.
pub const EMBEDDED_PUBKEY: &str = "b73bfc76a398ec3602313060a1bff3be7e61b06f0710d683d661aa1c9051f53e";

/// One signature error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningError {
    /// A hex key/seed file is not the expected length.
    BadHex { field: String, len: usize },
    /// Verification failed (bad key or tampered message).
    BadSignature,
}

impl std::fmt::Display for SigningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigningError::BadHex { field, len } => {
                write!(f, "invalid {field}: expected 64 hex chars, got {len}")
            }
            SigningError::BadSignature => write!(f, "signature verification failed"),
        }
    }
}

impl std::error::Error for SigningError {}

/// Generate a fresh ed25519 keypair: `(seed, public key)`, both 32
/// raw bytes.
pub fn keygen() -> (Vec<u8>, Vec<u8>) {
    use rand_core::OsRng;
    let sk = SigningKey::generate(&mut OsRng);
    (sk.to_bytes().to_vec(), sk.verifying_key().to_bytes().to_vec())
}

/// Sign `msg` with a 32-byte seed; returns the 64-byte signature.
pub fn sign(seed: &[u8], msg: &[u8]) -> Result<Vec<u8>, SigningError> {
    let sk = signing_key(seed)?;
    Ok(sk.sign(msg).to_bytes().to_vec())
}

/// Verify a 64-byte signature over `msg` with a 32-byte public key.
pub fn verify(pubkey: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), SigningError> {
    let vk = verification_key(pubkey)?;
    let s: [u8; 64] = sig
        .try_into()
        .map_err(|_| SigningError::BadHex {
            field: "signature".into(),
            len: sig.len(),
        })?;
    vk.verify(msg, &ed25519::Signature::from_bytes(&s))
        .map(|_| ())
        .map_err(|_| SigningError::BadSignature)
}

fn signing_key(seed: &[u8]) -> Result<SigningKey, SigningError> {
    let bytes: [u8; 32] = seed.try_into().map_err(|_| SigningError::BadHex {
        field: "signing key".into(),
        len: seed.len(),
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn verification_key(pubkey: &[u8]) -> Result<VerifyingKey, SigningError> {
    let bytes: [u8; 32] = pubkey.try_into().map_err(|_| SigningError::BadHex {
        field: "public key".into(),
        len: pubkey.len(),
    })?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| SigningError::BadSignature)
}

// ---------------------------------------------------------------------------
// Hex key/signature file helpers (the on-disk formats above).
// ---------------------------------------------------------------------------

/// Decode 64 lowercase/uppercase hex chars (whitespace-tolerant) into
/// 32 bytes.
pub fn hex_key(text: &str, field: &str) -> Result<Vec<u8>, SigningError> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != 64 {
        return Err(SigningError::BadHex {
            field: field.into(),
            len: clean.len(),
        });
    }
    let mut out = Vec::with_capacity(32);
    for i in (0..64).step_by(2) {
        let byte = u8::from_str_radix(&clean[i..i + 2], 16).map_err(|_| SigningError::BadHex {
            field: field.into(),
            len: clean.len(),
        })?;
        out.push(byte);
    }
    Ok(out)
}

/// Decode a 128-hex signature (64 bytes) from an `index.json.sig`
/// file (trailing newline tolerated).
pub fn hex_sig(text: &str) -> Result<Vec<u8>, SigningError> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != 128 {
        return Err(SigningError::BadHex {
            field: "index.json.sig".into(),
            len: clean.len(),
        });
    }
    let mut out = Vec::with_capacity(64);
    for i in (0..128).step_by(2) {
        let byte = u8::from_str_radix(&clean[i..i + 2], 16)
            .map_err(|_| SigningError::BadHex {
                field: "index.json.sig".into(),
                len: clean.len(),
            })?;
        out.push(byte);
    }
    Ok(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read a 32-byte key file (64-hex-char seed or public key, optional
/// trailing newline).
pub fn read_key_file(path: &std::path::Path, field: &str) -> Result<Vec<u8>, SigningError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SigningError::BadHex {
            field: format!("{field} ({e})"),
            len: 0,
        })?;
    hex_key(&text, field)
}

/// Write a 32-byte seed to `path` as 64 hex chars + newline, `0600`
/// permissions (the key is secret).
pub fn write_key_file(path: &std::path::Path, key: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, to_hex(key) + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keygen_sign_verify_roundtrip() {
        let (seed, pubkey) = keygen();
        assert_eq!(seed.len(), 32);
        assert_eq!(pubkey.len(), 32);
        let msg = b"[]";
        let sig = sign(&seed, msg).unwrap();
        assert_eq!(sig.len(), 64);
        assert!(verify(&pubkey, msg, &sig).is_ok());
    }

    #[test]
    fn verify_rejects_tamper_and_wrong_key() {
        let (seed, pubkey) = keygen();
        let (_s2, other) = keygen();
        let sig = sign(&seed, b"index.json bytes").unwrap();
        assert!(verify(&pubkey, b"index.json bytes ", &sig).is_err());
        assert!(verify(&other, b"index.json bytes", &sig).is_err());
    }

    #[test]
    fn hex_key_roundtrip() {
        let key = (0..32).collect::<Vec<u8>>();
        let text = to_hex(&key);
        assert_eq!(hex_key(&text, "k").unwrap(), key);
        // Whitespace/newline tolerant.
        let nl = format!("{text}\n");
        assert_eq!(hex_key(&nl, "k").unwrap(), key);
        assert!(hex_key("beef", "k").is_err());
    }

    #[test]
    fn hex_sig_roundtrip() {
        let sig = (0..64).collect::<Vec<u8>>();
        let text = to_hex(&sig) + "\n";
        assert_eq!(hex_sig(&text).unwrap(), sig);
        assert!(hex_sig("ab".into()).is_err());
    }
}
