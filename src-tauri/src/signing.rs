use base64::Engine;
use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{SigningKey, Signer};

/// Sign a message using Ed25519 private key (PEM format)
pub fn sign_message(private_key_pem: &str, message: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let signing_key = SigningKey::from_pkcs8_pem(private_key_pem)?;
    let signature = signing_key.sign(message.as_bytes());
    let b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
    Ok(b64)
}

/// Build the signed message string.
/// Format: METHOD\nPATH\nTIMESTAMP\nBODY
pub fn build_signed_message(method: &str, path: &str, timestamp: &str, body: &str) -> String {
    format!("{}\n{}\n{}\n{}", method, path, timestamp, body)
}
