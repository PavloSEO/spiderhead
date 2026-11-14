//! Release trust: the embedded signing key, the `SHA256SUMS` table and the Ed25519 check.
//!
//! The key file `packaging/release-key.pem` is the single switch: while it is empty signatures are
//! not checked (`unsigned`); once it holds a key every `SHA256SUMS` must carry a valid signature.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The release public key (PEM, SubjectPublicKeyInfo Ed25519); empty until releases are signed.
pub const RELEASE_KEY_PEM: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packaging/release-key.pem"
));

/// DER prefix of an Ed25519 SubjectPublicKeyInfo; the 32 key bytes follow.
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Whether `SHA256SUMS` signatures are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignaturePolicy {
    /// No key is embedded: signatures are not read.
    Disabled,
    /// Every `SHA256SUMS` needs a valid signature by this key.
    Required([u8; 32]),
}

/// The signature outcome reported to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureState {
    /// A valid signature by the release key.
    Verified,
    /// Signatures are switched off (no key embedded).
    Unsigned,
}

impl SignatureState {
    /// The spelling used in JSON answers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unsigned => "unsigned",
        }
    }
}

/// Failures of the trust chain.
#[derive(Debug, Error)]
pub enum TrustError {
    /// The key is not a PEM Ed25519 public key.
    #[error("the release key is not a PEM Ed25519 public key")]
    BadKey,
    /// A key is embedded but the release carries no signature.
    #[error("the release is not signed")]
    SignatureMissing,
    /// The signature does not match `SHA256SUMS`.
    #[error("the release signature is invalid")]
    SignatureInvalid,
    /// `SHA256SUMS` is malformed.
    #[error("malformed SHA256SUMS: {0}")]
    BadSums(String),
    /// `SHA256SUMS` has no line for the asset.
    #[error("SHA256SUMS has no entry for {0}")]
    NoEntry(String),
}

impl SignaturePolicy {
    /// The policy of this build: empty key file means `Disabled`.
    pub fn embedded() -> Self {
        Self::from_pem(RELEASE_KEY_PEM).unwrap_or(Self::Disabled)
    }

    /// Parse a PEM key; blank text means `Disabled`.
    pub fn from_pem(pem: &str) -> Result<Self, TrustError> {
        if pem.trim().is_empty() {
            return Ok(Self::Disabled);
        }
        let body: String = pem
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("-----") && !line.is_empty())
            .collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| TrustError::BadKey)?;
        let key = der
            .strip_prefix(&SPKI_PREFIX)
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .ok_or(TrustError::BadKey)?;
        Ok(Self::Required(key))
    }
}

/// Check `sums` against its detached signature under `policy`.
pub fn verify_sums(
    policy: &SignaturePolicy,
    sums: &[u8],
    sig: Option<&[u8]>,
) -> Result<SignatureState, TrustError> {
    match policy {
        SignaturePolicy::Disabled => Ok(SignatureState::Unsigned),
        SignaturePolicy::Required(key) => {
            let sig = sig.ok_or(TrustError::SignatureMissing)?;
            UnparsedPublicKey::new(&ED25519, key)
                .verify(sums, sig)
                .map_err(|_| TrustError::SignatureInvalid)?;
            Ok(SignatureState::Verified)
        }
    }
}

/// Name to lowercase SHA-256 hex, from a `SHA256SUMS` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sums(BTreeMap<String, String>);

impl Sums {
    /// The digest recorded for `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// All `(name, digest)` pairs, ordered by name.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, d)| (n.as_str(), d.as_str()))
    }
}

/// Parse `<hex64>  name` and `<hex64> *name` lines (CRLF accepted); a name listed twice with
/// different digests is an error.
pub fn parse_sums(text: &[u8]) -> Result<Sums, TrustError> {
    let text = std::str::from_utf8(text).map_err(|_| TrustError::BadSums("not UTF-8".into()))?;
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (digest, name) = line
            .split_once(' ')
            .ok_or_else(|| TrustError::BadSums(line.to_owned()))?;
        let name = name.strip_prefix(['*', ' ']).unwrap_or(name);
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(TrustError::BadSums(line.to_owned()));
        }
        if map
            .insert(name.to_owned(), digest.to_owned())
            .is_some_and(|old| old != digest)
        {
            return Err(TrustError::BadSums(format!(
                "conflicting entries for {name}"
            )));
        }
    }
    Ok(Sums(map))
}

/// Lowercase hex SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn pair() -> (Ed25519KeyPair, [u8; 32]) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("key");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("pair");
        let public: [u8; 32] = pair.public_key().as_ref().try_into().expect("32 bytes");
        (pair, public)
    }

    #[test]
    fn sums_parse_accepts_gnu_and_binary_markers() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text = format!("{a}  one\r\n{b} *two\n\n{a}  one\n");
        let sums = parse_sums(text.as_bytes()).expect("parse");
        assert_eq!(sums.get("one"), Some(a.as_str()));
        assert_eq!(sums.get("two"), Some(b.as_str()));
        assert_eq!(sums.get("three"), None);
        let conflict = format!("{a}  one\n{b}  one\n");
        assert!(parse_sums(conflict.as_bytes()).is_err());
        assert!(parse_sums(b"nonsense line\n").is_err());
        assert!(parse_sums(format!("{}  x\n", "A".repeat(64)).as_bytes()).is_err());
    }

    #[test]
    fn signature_required_rejects_missing_and_tampered() {
        let (pair, public) = pair();
        let policy = SignaturePolicy::Required(public);
        let sums = b"digest  file\n";
        let sig = pair.sign(sums);
        assert_eq!(
            verify_sums(&policy, sums, Some(sig.as_ref())).expect("valid"),
            SignatureState::Verified
        );
        assert!(matches!(
            verify_sums(&policy, b"digest  fil3\n", Some(sig.as_ref())),
            Err(TrustError::SignatureInvalid)
        ));
        assert!(matches!(
            verify_sums(&policy, sums, None),
            Err(TrustError::SignatureMissing)
        ));
    }

    #[test]
    fn empty_key_file_disables_signatures() {
        assert_eq!(
            RELEASE_KEY_PEM.trim().is_empty(),
            SignaturePolicy::embedded() == SignaturePolicy::Disabled
        );
        assert_eq!(
            SignaturePolicy::from_pem("\n").expect("blank"),
            SignaturePolicy::Disabled
        );
        let (_, public) = pair();
        let mut der = SPKI_PREFIX.to_vec();
        der.extend_from_slice(&public);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
        assert_eq!(
            SignaturePolicy::from_pem(&pem).expect("pem"),
            SignaturePolicy::Required(public)
        );
        let foreign = base64::engine::general_purpose::STANDARD.encode(b"not a spki");
        assert!(matches!(
            SignaturePolicy::from_pem(&format!(
                "-----BEGIN PUBLIC KEY-----\n{foreign}\n-----END PUBLIC KEY-----"
            )),
            Err(TrustError::BadKey)
        ));
    }

    #[test]
    fn get_sh_carries_the_same_public_key() {
        let script = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/get.sh"
        ));
        let start = script.find("PUBKEY='").expect("PUBKEY") + "PUBKEY='".len();
        let block = &script[start..start + script[start..].find('\'').expect("closing quote")];
        assert_eq!(block.trim(), RELEASE_KEY_PEM.trim());
    }
}
