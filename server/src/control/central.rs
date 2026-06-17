//! Prüfung zentraler Access-Tokens (Identity Provider srvhub.accessy.org).
//!
//! Das Token ist ein kompaktes EdDSA-JWT (siehe Hub `token.rs`). Wir prüfen es
//! OFFLINE mit dem veröffentlichten Public Key (`<url>/v2/keys`) — der Server
//! braucht dafür keine Verbindung zum Hub außer dem einmaligen Schlüsselabruf
//! beim Start.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

const ISSUER: &str = "srvhub.accessy.org";

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CentralClaims {
    /// central_uid
    pub sub: String,
    pub un: String,
    pub name: String,
    pub scope: String,
    /// freigegeben?
    pub apr: bool,
    pub tv: i64,
    pub iss: String,
    pub exp: i64,
}

#[derive(Clone)]
pub struct CentralVerifier {
    verifying: VerifyingKey,
}

impl CentralVerifier {
    /// Public Key laden: bevorzugt aus dem Hex-Override, sonst per HTTP von
    /// `<url>/v2/keys`.
    pub async fn load(url: &str, pubkey_hex_override: &str) -> anyhow::Result<Self> {
        let hex = if !pubkey_hex_override.trim().is_empty() {
            pubkey_hex_override.trim().to_string()
        } else {
            let endpoint = format!("{}/v2/keys", url.trim_end_matches('/'));
            let resp: serde_json::Value = reqwest::Client::new()
                .get(&endpoint)
                .send()
                .await?
                .json()
                .await?;
            resp.get("public_key_hex")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("Antwort von {} ohne public_key_hex", endpoint))?
        };
        let bytes = decode_hex(&hex)?;
        if bytes.len() != 32 {
            anyhow::bail!("Public Key hat nicht 32 Byte");
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let verifying = VerifyingKey::from_bytes(&arr)
            .map_err(|e| anyhow::anyhow!("Ungültiger Public Key: {}", e))?;
        Ok(Self { verifying })
    }

    /// Token prüfen (Signatur, alg, iss, exp) und Claims zurückgeben.
    pub fn verify(&self, token: &str) -> Result<CentralClaims, String> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err("malformed token".into());
        }
        let header_bytes = URL_SAFE_NO_PAD.decode(parts[0]).map_err(|_| "bad header b64")?;
        let header: Header = serde_json::from_slice(&header_bytes).map_err(|_| "bad header json")?;
        if header.alg != "EdDSA" {
            return Err("unexpected alg".into());
        }
        let sig_bytes = URL_SAFE_NO_PAD.decode(parts[2]).map_err(|_| "bad sig b64")?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| "bad sig len")?;
        let sig = Signature::from_bytes(&sig_arr);

        let signing_input = format!("{}.{}", parts[0], parts[1]);
        self.verifying
            .verify(signing_input.as_bytes(), &sig)
            .map_err(|_| "bad signature".to_string())?;

        let claim_bytes = URL_SAFE_NO_PAD.decode(parts[1]).map_err(|_| "bad claims b64")?;
        let claims: CentralClaims =
            serde_json::from_slice(&claim_bytes).map_err(|_| "bad claims json")?;

        if claims.iss != ISSUER {
            return Err("bad issuer".into());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if claims.exp <= now {
            return Err("expired".into());
        }
        Ok(claims)
    }
}

fn decode_hex(s: &str) -> anyhow::Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        anyhow::bail!("Hex-Länge ungerade");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow::anyhow!("Hex: {}", e)))
        .collect()
}
