//! Passphrase-protected credential vault.
//!
//! Adapter secrets (API keys, tokens) are encrypted with XChaCha20-Poly1305
//! using a key derived from the user's passphrase with Argon2id. Non-secret
//! adapter settings are stored in plaintext next to them. The derived key lives
//! only in memory and is zeroized on lock.

use std::path::Path;
use std::sync::{Mutex, RwLock};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use zeroize::Zeroizing;

const VERIFIER_PLAINTEXT: &[u8] = b"apex-vault-v1";
const VERIFIER_AAD: &[u8] = b"vault:verifier";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const MIN_PASSPHRASE_LEN: usize = 8;

// Argon2id parameters (OWASP baseline: 19 MiB, 2 iterations, 1 lane).
const KDF_MEMORY_KIB: u32 = 19_456;
const KDF_ITERATIONS: u32 = 2;
const KDF_LANES: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultStatus {
    Uninitialized,
    Locked,
    Unlocked,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterConfigRecord {
    pub adapter_id: String,
    pub enabled: bool,
    pub settings_json: String,
}

pub struct Vault {
    conn: Mutex<Connection>,
    key: RwLock<Option<Zeroizing<[u8; 32]>>>,
}

impl Vault {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("Failed to open vault: {e}"))?;
        Self::from_connection(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vault_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                salt BLOB NOT NULL,
                verifier_nonce BLOB NOT NULL,
                verifier_ct BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS adapter_configs (
                adapter_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 1,
                settings_json TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE IF NOT EXISTS adapter_secrets (
                adapter_id TEXT NOT NULL,
                field TEXT NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (adapter_id, field)
            );",
        )
        .map_err(|e| format!("Failed to initialize vault schema: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            key: RwLock::new(None),
        })
    }

    pub fn status(&self) -> VaultStatus {
        if self.key.read().expect("vault key lock poisoned").is_some() {
            return VaultStatus::Unlocked;
        }
        match self.has_meta() {
            Ok(true) => VaultStatus::Locked,
            _ => VaultStatus::Uninitialized,
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.status() == VaultStatus::Unlocked
    }

    fn has_meta(&self) -> Result<bool, String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        conn.query_row("SELECT 1 FROM vault_meta WHERE id = 1", [], |_| Ok(()))
            .optional()
            .map(|row| row.is_some())
            .map_err(|e| e.to_string())
    }

    /// Create the vault with a new passphrase and leave it unlocked.
    pub fn init(&self, passphrase: &str) -> Result<(), String> {
        validate_passphrase(passphrase)?;
        if self.has_meta()? {
            return Err("Vault is already initialized".into());
        }
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let key = derive_key(passphrase, &salt)?;
        let (nonce, ct) = encrypt(&key, VERIFIER_AAD, VERIFIER_PLAINTEXT)?;
        {
            let conn = self.conn.lock().expect("vault db lock poisoned");
            conn.execute(
                "INSERT INTO vault_meta (id, salt, verifier_nonce, verifier_ct) VALUES (1, ?1, ?2, ?3)",
                params![salt.to_vec(), nonce, ct],
            )
            .map_err(|e| e.to_string())?;
        }
        *self.key.write().expect("vault key lock poisoned") = Some(key);
        Ok(())
    }

    pub fn unlock(&self, passphrase: &str) -> Result<(), String> {
        let (salt, nonce, ct) = self.read_meta()?;
        let key = derive_key(passphrase, &salt)?;
        let plain = decrypt(&key, VERIFIER_AAD, &nonce, &ct)
            .map_err(|_| "Incorrect passphrase".to_string())?;
        if plain.as_slice() != VERIFIER_PLAINTEXT {
            return Err("Incorrect passphrase".into());
        }
        *self.key.write().expect("vault key lock poisoned") = Some(key);
        Ok(())
    }

    pub fn lock(&self) {
        *self.key.write().expect("vault key lock poisoned") = None;
    }

    /// Re-encrypt every secret under a key derived from `new_passphrase`.
    pub fn change_passphrase(&self, old: &str, new: &str) -> Result<(), String> {
        validate_passphrase(new)?;
        let (salt, nonce, ct) = self.read_meta()?;
        let old_key = derive_key(old, &salt)?;
        decrypt(&old_key, VERIFIER_AAD, &nonce, &ct)
            .map_err(|_| "Incorrect passphrase".to_string())?;

        let mut new_salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut new_salt);
        let new_key = derive_key(new, &new_salt)?;
        let (v_nonce, v_ct) = encrypt(&new_key, VERIFIER_AAD, VERIFIER_PLAINTEXT)?;

        let mut conn = self.conn.lock().expect("vault db lock poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, Vec<u8>, Vec<u8>)> = {
            let mut stmt = tx
                .prepare("SELECT adapter_id, field, nonce, ciphertext FROM adapter_secrets")
                .map_err(|e| e.to_string())?;
            let mapped = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|e| e.to_string())?;
            mapped.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        for (adapter_id, field, s_nonce, s_ct) in rows {
            let aad = secret_aad(&adapter_id, &field);
            let plain = decrypt(&old_key, &aad, &s_nonce, &s_ct)?;
            let (n, c) = encrypt(&new_key, &aad, &plain)?;
            tx.execute(
                "UPDATE adapter_secrets SET nonce = ?1, ciphertext = ?2 WHERE adapter_id = ?3 AND field = ?4",
                params![n, c, adapter_id, field],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "UPDATE vault_meta SET salt = ?1, verifier_nonce = ?2, verifier_ct = ?3 WHERE id = 1",
            params![new_salt.to_vec(), v_nonce, v_ct],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        drop(conn);
        *self.key.write().expect("vault key lock poisoned") = Some(new_key);
        Ok(())
    }

    fn read_meta(&self) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        conn.query_row(
            "SELECT salt, verifier_nonce, verifier_ct FROM vault_meta WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Vault is not initialized".to_string())
    }

    fn with_key<T>(&self, f: impl FnOnce(&[u8; 32]) -> Result<T, String>) -> Result<T, String> {
        let guard = self.key.read().expect("vault key lock poisoned");
        let key = guard.as_ref().ok_or_else(|| "Vault is locked".to_string())?;
        f(key)
    }

    pub fn set_secret(&self, adapter_id: &str, field: &str, value: &str) -> Result<(), String> {
        let aad = secret_aad(adapter_id, field);
        let (nonce, ct) = self.with_key(|key| encrypt(key, &aad, value.as_bytes()))?;
        let conn = self.conn.lock().expect("vault db lock poisoned");
        conn.execute(
            "INSERT INTO adapter_secrets (adapter_id, field, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(adapter_id, field) DO UPDATE SET
                nonce = excluded.nonce,
                ciphertext = excluded.ciphertext,
                updated_at = excluded.updated_at",
            params![adapter_id, field, nonce, ct, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_secret(&self, adapter_id: &str, field: &str) -> Result<Option<String>, String> {
        let row: Option<(Vec<u8>, Vec<u8>)> = {
            let conn = self.conn.lock().expect("vault db lock poisoned");
            conn.query_row(
                "SELECT nonce, ciphertext FROM adapter_secrets WHERE adapter_id = ?1 AND field = ?2",
                params![adapter_id, field],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?
        };
        let Some((nonce, ct)) = row else {
            return Ok(None);
        };
        let aad = secret_aad(adapter_id, field);
        let plain = self.with_key(|key| decrypt(key, &aad, &nonce, &ct))?;
        String::from_utf8(plain.to_vec())
            .map(Some)
            .map_err(|_| "Stored secret is not valid UTF-8".to_string())
    }

    /// Field names that have a stored secret. Works while locked (names only).
    pub fn secret_fields(&self, adapter_id: &str) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        let mut stmt = conn
            .prepare("SELECT field FROM adapter_secrets WHERE adapter_id = ?1 ORDER BY field")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![adapter_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn delete_secret(&self, adapter_id: &str, field: &str) -> Result<(), String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        conn.execute(
            "DELETE FROM adapter_secrets WHERE adapter_id = ?1 AND field = ?2",
            params![adapter_id, field],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_config(
        &self,
        adapter_id: &str,
        enabled: bool,
        settings_json: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        conn.execute(
            "INSERT INTO adapter_configs (adapter_id, enabled, settings_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(adapter_id) DO UPDATE SET
                enabled = excluded.enabled,
                settings_json = excluded.settings_json",
            params![adapter_id, enabled, settings_json],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_config(&self, adapter_id: &str) -> Result<Option<AdapterConfigRecord>, String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        conn.query_row(
            "SELECT adapter_id, enabled, settings_json FROM adapter_configs WHERE adapter_id = ?1",
            params![adapter_id],
            |r| {
                Ok(AdapterConfigRecord {
                    adapter_id: r.get(0)?,
                    enabled: r.get(1)?,
                    settings_json: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn list_configs(&self) -> Result<Vec<AdapterConfigRecord>, String> {
        let conn = self.conn.lock().expect("vault db lock poisoned");
        let mut stmt = conn
            .prepare("SELECT adapter_id, enabled, settings_json FROM adapter_configs ORDER BY adapter_id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(AdapterConfigRecord {
                    adapter_id: r.get(0)?,
                    enabled: r.get(1)?,
                    settings_json: r.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// Remove an adapter's config and all of its secrets.
    pub fn delete_adapter(&self, adapter_id: &str) -> Result<(), String> {
        let mut conn = self.conn.lock().expect("vault db lock poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM adapter_secrets WHERE adapter_id = ?1", params![adapter_id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM adapter_configs WHERE adapter_id = ?1", params![adapter_id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
}

fn validate_passphrase(passphrase: &str) -> Result<(), String> {
    if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
        return Err(format!(
            "Passphrase must be at least {MIN_PASSPHRASE_LEN} characters"
        ));
    }
    Ok(())
}

fn secret_aad(adapter_id: &str, field: &str) -> Vec<u8> {
    format!("secret:{adapter_id}:{field}").into_bytes()
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(KDF_MEMORY_KIB, KDF_ITERATIONS, KDF_LANES, Some(32))
        .map_err(|e| format!("Invalid KDF parameters: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| format!("Key derivation failed: {e}"))?;
    Ok(key)
}

fn encrypt(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| "Encryption failed".to_string())?;
    Ok((nonce.to_vec(), ct))
}

fn decrypt(
    key: &[u8; 32],
    aad: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    if nonce.len() != NONCE_LEN {
        return Err("Corrupt vault entry".into());
    }
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad })
        .map(Zeroizing::new)
        .map_err(|_| "Decryption failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_round_trip() {
        let v = Vault::open_in_memory().unwrap();
        assert_eq!(v.status(), VaultStatus::Uninitialized);
        v.init("correct horse").unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);

        v.set_secret("zerodha", "api_key", "abc123").unwrap();
        assert_eq!(v.get_secret("zerodha", "api_key").unwrap().as_deref(), Some("abc123"));
        assert_eq!(v.get_secret("zerodha", "missing").unwrap(), None);

        v.lock();
        assert_eq!(v.status(), VaultStatus::Locked);
        assert!(v.get_secret("zerodha", "api_key").is_err());
        assert_eq!(v.secret_fields("zerodha").unwrap(), vec!["api_key".to_string()]);

        assert!(v.unlock("wrong passphrase").is_err());
        assert_eq!(v.status(), VaultStatus::Locked);
        v.unlock("correct horse").unwrap();
        assert_eq!(v.get_secret("zerodha", "api_key").unwrap().as_deref(), Some("abc123"));
    }

    #[test]
    fn rejects_short_passphrase_and_double_init() {
        let v = Vault::open_in_memory().unwrap();
        assert!(v.init("short").is_err());
        v.init("long enough pass").unwrap();
        assert!(v.init("another long pass").is_err());
    }

    #[test]
    fn ciphertext_is_bound_to_adapter_and_field() {
        let v = Vault::open_in_memory().unwrap();
        v.init("correct horse").unwrap();
        v.set_secret("binance", "api_key", "k").unwrap();
        {
            let conn = v.conn.lock().unwrap();
            conn.execute(
                "UPDATE adapter_secrets SET adapter_id = 'coinbase' WHERE adapter_id = 'binance'",
                [],
            )
            .unwrap();
        }
        assert!(v.get_secret("coinbase", "api_key").is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let v = Vault::open_in_memory().unwrap();
        v.init("correct horse").unwrap();
        v.set_secret("groww", "api_key", "secret").unwrap();
        {
            let conn = v.conn.lock().unwrap();
            let mut ct: Vec<u8> = conn
                .query_row("SELECT ciphertext FROM adapter_secrets", [], |r| r.get(0))
                .unwrap();
            ct[0] ^= 0xff;
            conn.execute("UPDATE adapter_secrets SET ciphertext = ?1", params![ct]).unwrap();
        }
        assert!(v.get_secret("groww", "api_key").is_err());
    }

    #[test]
    fn change_passphrase_reencrypts_secrets() {
        let v = Vault::open_in_memory().unwrap();
        v.init("old passphrase").unwrap();
        v.set_secret("zerodha", "api_secret", "s3cret").unwrap();
        assert!(v.change_passphrase("wrong one!!", "new passphrase").is_err());
        v.change_passphrase("old passphrase", "new passphrase").unwrap();
        v.lock();
        assert!(v.unlock("old passphrase").is_err());
        v.unlock("new passphrase").unwrap();
        assert_eq!(v.get_secret("zerodha", "api_secret").unwrap().as_deref(), Some("s3cret"));
    }

    #[test]
    fn config_crud_and_delete_adapter() {
        let v = Vault::open_in_memory().unwrap();
        v.init("correct horse").unwrap();
        v.set_config("zerodha", true, r#"{"redirect_url":"http://127.0.0.1"}"#).unwrap();
        v.set_secret("zerodha", "api_key", "k").unwrap();
        let cfg = v.get_config("zerodha").unwrap().unwrap();
        assert!(cfg.enabled);
        assert_eq!(v.list_configs().unwrap().len(), 1);
        v.delete_adapter("zerodha").unwrap();
        assert!(v.get_config("zerodha").unwrap().is_none());
        assert!(v.secret_fields("zerodha").unwrap().is_empty());
    }
}
