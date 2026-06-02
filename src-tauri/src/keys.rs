use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::SigningKey;
use std::fs;
use std::path::PathBuf;

/// Get the config directory for key storage
fn config_dir() -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rpi-infodisplay");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Get or create Ed25519 keypair. Returns (private_key_pem, public_key_pem).
/// Both are proper PEM-encoded (PKCS#8 private, SPKI public) — compatible with
/// Node.js crypto and WebCrypto importKey("spki", ...).
pub fn get_or_create_keys() -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let dir = config_dir()?;
    let key_path = dir.join("device.key");
    let pub_path = dir.join("device.pub");

    if key_path.exists() && pub_path.exists() {
        let private_pem = fs::read_to_string(&key_path)?;
        let public_pem = fs::read_to_string(&pub_path)?;
        return Ok((private_pem, public_pem));
    }

    // Generate new keypair
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let verifying_key = signing_key.verifying_key();

    // Encode private key as PKCS#8 PEM (same as Node.js type: 'pkcs8', format: 'pem')
    let private_pem = signing_key.to_pkcs8_pem(pkcs8::LineEnding::LF)?;

    // Encode public key as SPKI PEM (same as Node.js type: 'spki', format: 'pem')
    let public_pem = verifying_key.to_public_key_pem(pkcs8::LineEnding::LF)?;

    fs::write(&key_path, private_pem.as_bytes())?;
    fs::write(&pub_path, public_pem.as_bytes())?;

    log::info!("[keys] Generated new Ed25519 keypair");

    Ok((private_pem.to_string(), public_pem))
}

/// Load device ID from file, if it exists
pub fn load_device_id() -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let dir = config_dir()?;
    let id_path = dir.join("device-id");

    if id_path.exists() {
        let id = fs::read_to_string(&id_path)?;
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Save device ID to file
pub fn save_device_id(id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = config_dir()?;
    let id_path = dir.join("device-id");
    fs::write(&id_path, id)?;
    log::info!("[keys] Device ID saved: {}", id);
    Ok(())
}
