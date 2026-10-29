//! Google service-account JWT bearer grant (`urn:ietf:params:oauth:grant-type:jwt-bearer`) without
//! a Google SDK: the RS256 assertion is signed with `ring` and exchanged over the injected transport.
//!
//! Every failure maps to `MissingCredential`, so diagnostics stay coarse and never carry key material.

use serde_json::{json, Value};
use spiderhead_core::host::Host;
use std::path::Path;

use super::SERVICE_ACCOUNT_ERRORS;
use crate::credentials::{self, CredentialError};
use crate::http::{urlencode, Request, Transport, TransportError};
use crate::{pyjson, pytime};

/// The JWT-bearer grant type `oauth2` sends for service accounts.
const JWT_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Requested token lifetime: one hour.
const TOKEN_LIFETIME_SECONDS: i64 = 3600;

fn missing(message: &str) -> CredentialError {
    CredentialError::Missing(message.to_owned())
}

/// Any refresh failure collapses into the same coarse `MissingCredential`.
fn refresh_failed() -> CredentialError {
    missing("GSC service-account token refresh failed; check file permissions and Google grants")
}

/// Unpadded base64url encoding.
fn b64url(data: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
        for i in 0..=chunk.len() {
            let shift = chunk.len() as i32 * 8 - 6 * (i as i32 + 1);
            let six = if shift >= 0 {
                (n >> shift) & 63
            } else {
                (n << (-shift) as u32) & 63
            };
            out.push(ALPHA[six as usize] as char);
        }
    }
    out
}

/// PEM `"-----BEGIN PRIVATE KEY-----"`/`"-----BEGIN RSA PRIVATE KEY-----"` → DER bytes.
fn pem_der(pem: &str) -> Result<Vec<u8>, CredentialError> {
    let mut der = String::new();
    let mut inside = false;
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN ") {
            inside = true;
            continue;
        }
        if line.starts_with("-----END ") {
            inside = false;
            continue;
        }
        if inside {
            der.push_str(line);
        }
    }
    if der.is_empty() {
        return Err(refresh_failed());
    }
    // Reject PKCS#1 `BEGIN RSA PRIVATE KEY`: `ring` only accepts PKCS#8 keys.
    if !pem.contains("BEGIN PRIVATE KEY") {
        return Err(refresh_failed());
    }
    fn value_of(b: u8) -> Result<u32, CredentialError> {
        match b {
            b'A'..=b'Z' => Ok(u32::from(b - b'A')),
            b'a'..=b'z' => Ok(u32::from(b - b'a' + 26)),
            b'0'..=b'9' => Ok(u32::from(b - b'0' + 52)),
            b'+' => Ok(62),
            b'/' => Ok(63),
            b'=' => Ok(0),
            _ => Err(refresh_failed()),
        }
    }
    let raw: Vec<u8> = der.bytes().collect();
    let mut out = Vec::with_capacity(raw.len() * 3 / 4);
    for chunk in raw.chunks(4) {
        let mut acc = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            acc |= value_of(b)? << (18 - i * 6);
        }
        let bytes = [acc >> 16, acc >> 8, acc].map(|b| (b & 0xff) as u8);
        let take = chunk
            .iter()
            .filter(|&&b| b != b'=')
            .count()
            .saturating_sub(1);
        out.extend_from_slice(&bytes[..take.min(3)]);
    }
    Ok(out)
}

/// Sign `signing_input` with the PKCS#8 PEM RSA key (`RS256`).
fn sign_rs256(private_key_pem: &str, signing_input: &str) -> Result<String, CredentialError> {
    use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
    let der = pem_der(private_key_pem)?;
    let key = RsaKeyPair::from_pkcs8(&der).map_err(|_| refresh_failed())?;
    let rng = ring::rand::SystemRandom::new();
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &rng,
        signing_input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| refresh_failed())?;
    Ok(b64url(&signature))
}

/// `service_account_access_token` — mint and exchange one read-only access token.
///
/// `scope` is the product scope the caller requests (GSC passes `READONLY_SCOPE`).
pub async fn service_account_access_token<T: Transport>(
    host: &dyn Host,
    root: &Path,
    scope: &str,
    transport: &T,
) -> Result<String, CredentialError> {
    service_account_access_token_at(
        host,
        root,
        scope,
        pytime::now_micros() / 1_000_000,
        transport,
    )
    .await
}

/// [`service_account_access_token`] with pinned `iat`/`exp` times.
/// Tests use it to make the signed assertion reproducible.
pub async fn service_account_access_token_at<T: Transport>(
    host: &dyn Host,
    root: &Path,
    scope: &str,
    now_seconds: i64,
    transport: &T,
) -> Result<String, CredentialError> {
    let (status, info) = credentials::gsc_service_account_document(host, root);
    let Some(info) = info else {
        let message = SERVICE_ACCOUNT_ERRORS
            .iter()
            .find(|(key, _)| *key == status.as_str())
            .map(|(_, m)| *m)
            .unwrap_or("GSC service-account JSON file is not configured or readable");
        return Err(missing(message));
    };
    let get_str = |key: &str| info.get(key).and_then(Value::as_str).unwrap_or_default();
    let (client_email, private_key, token_uri) = (
        get_str("client_email"),
        get_str("private_key"),
        get_str("token_uri"),
    );
    let claims = json!({
        "iss": client_email,
        "scope": scope,
        "aud": token_uri,
        "iat": now_seconds,
        "exp": now_seconds + TOKEN_LIFETIME_SECONDS,
    });
    let header = json!({"alg": "RS256", "typ": "JWT"});
    let signing_input = format!(
        "{}.{}",
        b64url(pyjson::dumps_opts(&header, true, false).as_bytes()),
        b64url(pyjson::dumps_opts(&claims, true, false).as_bytes()),
    );
    let assertion = format!(
        "{signing_input}.{}",
        sign_rs256(private_key, &signing_input)?
    );
    let fields = [
        ("grant_type".to_owned(), JWT_GRANT.to_owned()),
        ("assertion".to_owned(), assertion),
    ];
    let request = Request::new("POST", token_uri)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(urlencode(&fields).into_bytes())
        .timeout(30.0);
    let body: Value = match transport.send(&request).await {
        Ok(response) => serde_json::from_slice(&response.body).map_err(|_| refresh_failed())?,
        Err(TransportError::Http(_))
        | Err(TransportError::Connect(_))
        | Err(TransportError::InvalidRequest(_)) => return Err(refresh_failed()),
    };
    match body.get("access_token").and_then(Value::as_str) {
        Some(token) if !token.is_empty() => Ok(token.to_owned()),
        _ => Err(missing(
            "GSC service-account token refresh returned no access token",
        )),
    }
}
