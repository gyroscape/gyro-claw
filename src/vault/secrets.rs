//! # Secure Secret Vault
//!
//! AES-256-GCM encrypted secret storage with Argon2 key derivation.
//! Secrets are stored encrypted on disk and can only be accessed by the executor.
//! The AI model NEVER sees secret values — it only knows that secrets exist by key name.
//!
//! Fingerprints use HMAC-SHA256 with an HKDF-derived key (never the raw master key).
//! All decrypted buffers are zeroized after use.
//!
//! Storage layout:
//! ```text
//! ~/.gyro-claw/vault/
//!   vault.enc     — encrypted secret data
//!   meta.json     — vault metadata (version, key count, created at)
//! ```

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use argon2::Argon2;
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use zeroize::Zeroize;

use super::telemetry::{derive_fingerprint_key, hmac_fingerprint, hmac_fingerprint_tokens};

/// Salt length for Argon2 key derivation
const SALT_LEN: usize = 16;
/// Nonce length for AES-256-GCM
const NONCE_LEN: usize = 12;
/// Current vault format version
const VAULT_VERSION: u32 = 3;

#[derive(Debug, Clone)]
pub struct SecretRecord {
    pub key: String,
    pub value: String,
    pub scope: String,
    pub fingerprint: String,
    pub token_fingerprints: Vec<String>,
}

/// Legacy encrypted vault file structure (v1): one nonce for the full payload.
#[derive(Serialize, Deserialize)]
struct VaultFileV1 {
    salt: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Encrypted entry in vault v2/v3.
#[derive(Serialize, Deserialize)]
struct EncryptedSecretBlob {
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Encrypted vault file structure (v2): per-secret authenticated ciphertexts.
#[derive(Serialize, Deserialize)]
struct VaultFileV2 {
    version: u32,
    salt: Vec<u8>,
    #[serde(default)]
    fingerprints: HashMap<String, String>,
    secrets: HashMap<String, EncryptedSecretBlob>,
}

/// Encrypted vault file structure (v3): HMAC fingerprints + per-secret scopes.
#[derive(Serialize, Deserialize)]
struct VaultFileV3 {
    version: u32,
    salt: Vec<u8>,
    #[serde(default)]
    fingerprints: HashMap<String, String>,
    #[serde(default)]
    scopes: HashMap<String, String>,
    secrets: HashMap<String, EncryptedSecretBlob>,
}

/// Vault metadata stored alongside the encrypted data.
#[derive(Serialize, Deserialize)]
struct VaultMeta {
    /// Vault format version
    version: u32,
    /// Number of secrets stored
    secret_count: usize,
    /// When the vault was first created
    created_at: String,
    /// When the vault was last modified
    updated_at: String,
}

/// In-memory representation of decrypted secrets
#[derive(Serialize, Deserialize, Default)]
struct VaultData {
    secrets: HashMap<String, String>,
    #[serde(default)]
    scopes: HashMap<String, String>,
}

/// The SecretVault manages encrypted secret storage.
/// Secrets are protected with AES-256-GCM encryption and Argon2id key derivation.
pub struct SecretVault {
    vault_path: PathBuf,
    meta_path: PathBuf,
    master_key: Vec<u8>,
    salt: Vec<u8>,
    fingerprint_key: [u8; 32],
}

impl Drop for SecretVault {
    fn drop(&mut self) {
        self.master_key.zeroize();
        self.fingerprint_key.zeroize();
    }
}

impl SecretVault {
    /// Create or open a vault with the given master password.
    /// The master password is used to derive the encryption key via Argon2id.
    pub fn new(master_password: &str) -> Result<Self> {
        let vault_dir = vault_directory();
        std::fs::create_dir_all(&vault_dir).context("Failed to create vault directory")?;

        let vault_path = vault_dir.join("vault.enc");
        let meta_path = vault_dir.join("meta.json");

        let mut salt = [0u8; SALT_LEN];
        let mut has_existing_salt = false;
        if let Some(existing_salt) = read_existing_salt(&vault_path)? {
            if existing_salt.len() == SALT_LEN {
                salt.copy_from_slice(&existing_salt);
                has_existing_salt = true;
            }
        }

        if !has_existing_salt {
            OsRng.fill_bytes(&mut salt);
        }

        // Derive a 32-byte key from the master password using Argon2id.
        let master_key = derive_key_from_password(master_password, &salt)?;
        let fingerprint_key = derive_fingerprint_key(&master_key);

        let vault = Self {
            vault_path,
            meta_path,
            master_key,
            salt: salt.to_vec(),
            fingerprint_key,
        };

        // Initialize metadata if it doesn't exist.
        if !vault.meta_path.exists() {
            vault.save_meta(0)?;
        }

        // Opportunistic migration: if v1 or v2 exists, decrypt + re-save into v3.
        if vault.vault_path.exists() {
            let format = detect_vault_format(&vault.vault_path)?;
            if format < VAULT_VERSION {
                // Backup before migration
                let backup_path = vault.vault_path.with_extension("enc.bak");
                std::fs::copy(&vault.vault_path, &backup_path)
                    .context("Failed to create vault backup before migration")?;

                match vault.load_data() {
                    Ok(data) => {
                        if let Err(e) = vault.save_data(&data) {
                            // Migration failed — restore backup
                            if backup_path.exists() {
                                std::fs::copy(&backup_path, &vault.vault_path).ok();
                            }
                            return Err(e);
                        }
                        // Migration succeeded — remove backup
                        std::fs::remove_file(&backup_path).ok();
                    }
                    Err(e) => {
                        // Restore backup on load failure
                        if backup_path.exists() {
                            std::fs::copy(&backup_path, &vault.vault_path).ok();
                        }
                        return Err(e);
                    }
                }
            }
        }

        Ok(vault)
    }

    /// Store a secret with the given key name and optional scope.
    /// If a secret with this key already exists, it is overwritten.
    pub fn store_secret(&self, key: &str, value: &str) -> Result<()> {
        self.store_secret_with_scope(key, value, "default")
    }

    /// Store a secret with an explicit scope (e.g. "github", "openai").
    pub fn store_secret_with_scope(&self, key: &str, value: &str, scope: &str) -> Result<()> {
        let mut data = self.load_data().unwrap_or_default();
        data.secrets.insert(key.to_string(), value.to_string());
        data.scopes.insert(key.to_string(), scope.to_string());
        self.save_data(&data)?;
        self.save_meta(data.secrets.len())?;
        Ok(())
    }

    /// Retrieve a secret by key name.
    /// Returns `None` if the key does not exist.
    pub fn get_secret(&self, key: &str) -> Result<Option<String>> {
        let data = self.load_data()?;
        Ok(data.secrets.get(key).cloned())
    }

    /// Remove a secret by key name.
    pub fn remove_secret(&self, key: &str) -> Result<()> {
        let mut data = self.load_data().unwrap_or_default();
        data.secrets.remove(key);
        data.scopes.remove(key);
        self.save_data(&data)?;
        self.save_meta(data.secrets.len())?;
        Ok(())
    }

    /// List all secret key names (values are NEVER returned).
    /// This is safe to show to the AI model.
    pub fn list_secret_keys(&self) -> Result<Vec<String>> {
        let data = self.load_data().unwrap_or_default();
        let mut keys: Vec<String> = data.secrets.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }

    /// Return decrypted records with HMAC-based leak-detection fingerprints.
    pub fn list_secret_records(&self) -> Result<Vec<SecretRecord>> {
        let data = self.load_data()?;
        let mut records = Vec::new();
        for (key, value) in data.secrets {
            let scope = data
                .scopes
                .get(&key)
                .cloned()
                .unwrap_or_else(|| "default".to_string());
            records.push(SecretRecord {
                key,
                fingerprint: hmac_fingerprint(value.as_bytes(), &self.fingerprint_key),
                token_fingerprints: hmac_fingerprint_tokens(&value, &self.fingerprint_key),
                scope,
                value,
            });
        }
        Ok(records)
    }

    pub fn format_version(&self) -> u32 {
        VAULT_VERSION
    }

    pub fn verify_integrity(&self) -> Result<()> {
        let _ = self.load_data()?;
        Ok(())
    }

    /// Expose the HKDF-derived fingerprint key for executor-side fingerprinting.
    pub(crate) fn fingerprint_key(&self) -> &[u8; 32] {
        &self.fingerprint_key
    }

    /// Load and decrypt vault data from disk.
    fn load_data(&self) -> Result<VaultData> {
        if !self.vault_path.exists() {
            return Ok(VaultData::default());
        }

        let file_bytes = std::fs::read(&self.vault_path).context("Failed to read vault file")?;

        // Try v3 first, then v2, then v1
        if let Ok(vault_file) = serde_json::from_slice::<VaultFileV3>(&file_bytes) {
            if vault_file.version >= 3 {
                return self.decrypt_v3(vault_file);
            }
        }

        if let Ok(vault_file) = serde_json::from_slice::<VaultFileV2>(&file_bytes) {
            return self.decrypt_v2(vault_file);
        }

        if let Ok(vault_file) = serde_json::from_slice::<VaultFileV1>(&file_bytes) {
            return self.decrypt_v1(vault_file);
        }

        anyhow::bail!("Failed to parse vault file (unsupported format)")
    }

    /// Encrypt and save vault data to disk using v3 structure.
    fn save_data(&self, data: &VaultData) -> Result<()> {
        let cipher = Aes256Gcm::new_from_slice(&self.master_key)
            .map_err(|_| anyhow::anyhow!("Failed to create cipher — invalid key length"))?;

        let mut encrypted_secrets = HashMap::new();
        let mut fingerprints = HashMap::new();
        for (key, value) in &data.secrets {
            let mut nonce_bytes = [0u8; NONCE_LEN];
            OsRng.fill_bytes(&mut nonce_bytes);
            let nonce = Nonce::from_slice(&nonce_bytes);

            let ciphertext = cipher
                .encrypt(nonce, value.as_bytes())
                .map_err(|_| anyhow::anyhow!("Encryption failed"))?;

            encrypted_secrets.insert(
                key.to_string(),
                EncryptedSecretBlob {
                    nonce: nonce_bytes.to_vec(),
                    ciphertext,
                },
            );
            fingerprints.insert(
                key.to_string(),
                hmac_fingerprint(value.as_bytes(), &self.fingerprint_key),
            );
        }

        let vault_file = VaultFileV3 {
            version: VAULT_VERSION,
            salt: self.salt.clone(),
            fingerprints,
            scopes: data.scopes.clone(),
            secrets: encrypted_secrets,
        };

        let file_bytes =
            serde_json::to_vec_pretty(&vault_file).context("Failed to serialize vault file")?;
        std::fs::write(&self.vault_path, file_bytes).context("Failed to write vault file")?;

        Ok(())
    }

    fn decrypt_v3(&self, vault_file: VaultFileV3) -> Result<VaultData> {
        let VaultFileV3 {
            version,
            fingerprints,
            scopes,
            secrets: encrypted_secrets,
            ..
        } = vault_file;

        if version < 3 {
            anyhow::bail!("Unsupported vault version: {}", version);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.master_key)
            .map_err(|_| anyhow::anyhow!("Failed to create cipher — invalid key length"))?;

        let mut secrets = HashMap::new();
        for (key, blob) in encrypted_secrets {
            if blob.nonce.len() != NONCE_LEN {
                anyhow::bail!("Invalid nonce length in vault entry '{}'", key);
            }
            let nonce = Nonce::from_slice(&blob.nonce);
            let mut plaintext = cipher
                .decrypt(nonce, blob.ciphertext.as_ref())
                .map_err(|_| anyhow::anyhow!("Decryption failed — wrong master password?"))?;

            let value = String::from_utf8(plaintext.clone())
                .with_context(|| format!("Failed to parse decrypted UTF-8 for secret '{}'", key))?;

            // Verify HMAC fingerprint integrity
            if let Some(expected_fp) = fingerprints.get(&key) {
                let actual_fp = hmac_fingerprint(value.as_bytes(), &self.fingerprint_key);
                if &actual_fp != expected_fp {
                    anyhow::bail!("Integrity check failed for secret '{}'", key);
                }
            }

            // Zeroize plaintext buffer
            plaintext.zeroize();

            secrets.insert(key, value);
        }

        Ok(VaultData { secrets, scopes })
    }

    fn decrypt_v2(&self, vault_file: VaultFileV2) -> Result<VaultData> {
        let VaultFileV2 {
            version,
            secrets: encrypted_secrets,
            ..
        } = vault_file;

        if version < 2 {
            anyhow::bail!("Unsupported vault version: {}", version);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.master_key)
            .map_err(|_| anyhow::anyhow!("Failed to create cipher — invalid key length"))?;

        let mut secrets = HashMap::new();
        for (key, blob) in encrypted_secrets {
            if blob.nonce.len() != NONCE_LEN {
                anyhow::bail!("Invalid nonce length in vault entry '{}'", key);
            }
            let nonce = Nonce::from_slice(&blob.nonce);
            let mut plaintext = cipher
                .decrypt(nonce, blob.ciphertext.as_ref())
                .map_err(|_| anyhow::anyhow!("Decryption failed — wrong master password?"))?;

            let value = String::from_utf8(plaintext.clone())
                .with_context(|| format!("Failed to parse decrypted UTF-8 for secret '{}'", key))?;
            plaintext.zeroize();

            secrets.insert(key, value);
        }

        // v2 has no scopes; default all to "default"
        Ok(VaultData {
            secrets,
            scopes: HashMap::new(),
        })
    }

    fn decrypt_v1(&self, vault_file: VaultFileV1) -> Result<VaultData> {
        let cipher = Aes256Gcm::new_from_slice(&self.master_key)
            .map_err(|_| anyhow::anyhow!("Failed to create cipher — invalid key length"))?;
        let nonce = Nonce::from_slice(&vault_file.nonce);

        let mut plaintext = cipher
            .decrypt(nonce, vault_file.ciphertext.as_ref())
            .map_err(|_| anyhow::anyhow!("Decryption failed — wrong master password?"))?;

        let data: VaultData =
            serde_json::from_slice(&plaintext).context("Failed to parse decrypted vault data")?;
        plaintext.zeroize();
        Ok(data)
    }

    /// Save vault metadata (secret count, timestamps).
    fn save_meta(&self, secret_count: usize) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        let meta = if self.meta_path.exists() {
            let existing: VaultMeta =
                serde_json::from_slice(&std::fs::read(&self.meta_path).unwrap_or_default())
                    .unwrap_or(VaultMeta {
                        version: VAULT_VERSION,
                        secret_count,
                        created_at: now.clone(),
                        updated_at: now.clone(),
                    });

            VaultMeta {
                version: VAULT_VERSION,
                secret_count,
                created_at: existing.created_at,
                updated_at: now,
            }
        } else {
            VaultMeta {
                version: VAULT_VERSION,
                secret_count,
                created_at: now.clone(),
                updated_at: now,
            }
        };

        let bytes = serde_json::to_vec_pretty(&meta).context("Failed to serialize vault meta")?;
        std::fs::write(&self.meta_path, bytes).context("Failed to write vault meta")?;

        Ok(())
    }
}

/// Derive a 32-byte encryption key from a password and salt using Argon2id.
fn derive_key_from_password(password: &str, salt: &[u8]) -> Result<Vec<u8>> {
    let mut key = vec![0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Argon2 key derivation failed: {}", e))?;

    Ok(key)
}

fn read_existing_salt(vault_path: &PathBuf) -> Result<Option<Vec<u8>>> {
    if !vault_path.exists() {
        return Ok(None);
    }

    let file_bytes = std::fs::read(vault_path).context("Failed to read vault file")?;
    if let Ok(v3) = serde_json::from_slice::<VaultFileV3>(&file_bytes) {
        return Ok(Some(v3.salt));
    }
    if let Ok(v2) = serde_json::from_slice::<VaultFileV2>(&file_bytes) {
        return Ok(Some(v2.salt));
    }
    if let Ok(v1) = serde_json::from_slice::<VaultFileV1>(&file_bytes) {
        return Ok(Some(v1.salt));
    }

    Ok(None)
}

/// Detect the vault format version from the file.
fn detect_vault_format(vault_path: &PathBuf) -> Result<u32> {
    if !vault_path.exists() {
        return Ok(0);
    }

    let file_bytes = std::fs::read(vault_path).context("Failed to read vault file")?;

    if let Ok(v3) = serde_json::from_slice::<VaultFileV3>(&file_bytes) {
        if v3.version >= 3 {
            return Ok(v3.version);
        }
    }
    if let Ok(v2) = serde_json::from_slice::<VaultFileV2>(&file_bytes) {
        if v2.version >= 2 {
            return Ok(v2.version);
        }
    }
    if serde_json::from_slice::<VaultFileV1>(&file_bytes).is_ok() {
        return Ok(1);
    }

    Ok(0)
}

/// Get the vault directory: `~/.gyro-claw/vault/`
fn vault_directory() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".gyro-claw").join("vault"))
        .unwrap_or_else(|| PathBuf::from(".gyro-claw/vault"))
}
